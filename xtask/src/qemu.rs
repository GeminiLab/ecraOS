//! QEMU guest execution.
//!
//! Expands command options into concrete runs, records guest logs, and reports every outcome.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    TaskResult, build,
    cli::{Cell, QemuArgs, Target},
    process,
};

#[cfg(test)]
#[path = "qemu_tests.rs"]
mod tests;

/// The outcome of a guest run.
///
/// Separates build errors and timeouts from guest failures for vector summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// A successful guest run.
    ///
    /// Requires a zero exit status and no panic or fatal output.
    Pass,
    /// An application or loader build failure.
    ///
    /// QEMU is never started for this cell.
    BuildFailure,
    /// An expired guest timeout.
    ///
    /// The guest has been terminated and reaped.
    Timeout,
    /// A guest failure or unexpected termination.
    ///
    /// Includes fatal output even when QEMU exits successfully.
    Fail,
}

impl Outcome {
    /// Returns the public result label.
    ///
    /// Labels match the classifications used by the former shell scripts.
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::BuildFailure => "BUILD_FAILURE",
            Self::Timeout => "TIMEOUT",
            Self::Fail => "FAIL",
        }
    }

    /// Returns the exit code for one run.
    ///
    /// Vector runs aggregate all failures to status 1 after reporting individual statuses.
    fn code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::BuildFailure => 2,
            Self::Timeout => 124,
            Self::Fail => 1,
        }
    }
}

/// A completed QEMU cell.
///
/// Associates the result with its original options and diagnostic log.
struct Report {
    /// The concrete guest configuration.
    ///
    /// Contains scalar values suitable for a result table.
    cell: Cell,
    /// The execution outcome.
    ///
    /// Distinguishes build, timeout, and guest failures.
    outcome: Outcome,
    /// The log file for this invocation.
    ///
    /// Retained even when the build or guest fails.
    log_path: PathBuf,
}

/// Selects the QEMU executable and architecture options.
///
/// Matches the existing machine and CPU configurations for each guest architecture.
fn command(target: Target) -> Command {
    match target {
        Target::X86_64 => {
            let mut command = Command::new("qemu-system-x86_64");
            command.args(["-cpu", "qemu64,+fsgsbase,+x2apic"]);
            command
        }
        Target::Riscv64 => {
            let mut command = Command::new("qemu-system-riscv64");
            command.args(["-machine", "virt"]);
            command
        }
        Target::Aarch64 => {
            let mut command = Command::new("qemu-system-aarch64");
            command.args(["-machine", "virt,gic-version=3", "-cpu", "cortex-a72"]);
            command
        }
    }
}

/// Detects guest panic and fatal messages.
///
/// Inspects only guest output so build diagnostics cannot turn a successful guest into a failure.
fn fatal(output: &[u8]) -> bool {
    String::from_utf8_lossy(output)
        .to_lowercase()
        .lines()
        .any(|line| {
            line.contains("kernel panic at ")
                || line.contains("panic at ")
                || line.contains("bug at ")
                || line.contains("fatal:")
                || line.contains("triple fault")
                || (line.contains("unhandled supervisor ") && line.contains("fault"))
        })
}

/// Creates a unique guest log.
///
/// Names include the application, architecture, profile, CPU count, time, and xtask process ID.
fn log_file(root: &Path, cell: &Cell) -> TaskResult<(File, PathBuf)> {
    let log_dir = root.join("target/qemu-logs");
    fs::create_dir_all(&log_dir)?;
    let app = cell
        .build
        .crate_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let app: String = app
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = log_dir.join(format!(
        "{stamp}-{}-{app}-{}-{}-{}.log",
        std::process::id(),
        cell.build.target.triple(),
        cell.build.profile.directory(),
        cell.cpus
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&path)?;
    Ok((file, path))
}

/// Executes one build and guest run.
///
/// Converts expected build and guest failures into report values so later vector cells still run.
fn run_one(
    root: &Path,
    cell: Cell,
    timeout: Duration,
    extra_args: &[OsString],
) -> TaskResult<Report> {
    let (mut log, log_path) = log_file(root, &cell)?;
    eprintln!("QEMU log: {}", log_path.display());
    let header = format!(
        "Crate: {}, target: {}, profile: {}, CPUs: {}",
        cell.build.crate_path.display(),
        cell.build.target.triple(),
        cell.build.profile.directory(),
        cell.cpus
    );
    println!("{header}");
    writeln!(log, "{header}")?;
    let (outcome, reason) = match build::image(root, &cell.build, Some(&log)) {
        Err(error) => (Outcome::BuildFailure, error.to_string()),
        Ok(images) => {
            let image = if cell.build.target == Target::Aarch64 {
                images.loader_binary
            } else {
                images.loader
            };
            writeln!(log, "Loader: {}", image.display())?;
            let mut command = command(cell.build.target);
            command
                .current_dir(root)
                .arg("-smp")
                .arg(cell.cpus.to_string())
                .args(["-nographic", "-no-reboot", "-kernel"])
                .arg(&image)
                .args(extra_args);
            if !image.is_file() {
                (
                    Outcome::BuildFailure,
                    format!("kernel image not found: {}", image.display()),
                )
            } else {
                match process::guest(&mut command, &log, &log_path, timeout) {
                    Err(error) => (Outcome::Fail, error.to_string()),
                    Ok((_, true, _)) => (Outcome::Timeout, format!("after {}s", timeout.as_secs())),
                    Ok((status, false, output)) if fatal(&output) => {
                        (Outcome::Fail, format!("GUEST PANIC/FATAL ({status})"))
                    }
                    Ok((status, false, _)) if status.success() => {
                        (Outcome::Pass, format!("GUEST SUCCESS ({status})"))
                    }
                    Ok((status, false, _)) => (
                        Outcome::Fail,
                        format!("UNKNOWN GUEST TERMINATION ({status})"),
                    ),
                }
            }
        }
    };
    let result = format!("{}: {reason}, log: {}", outcome.label(), log_path.display());
    writeln!(log, "{result}")?;
    eprintln!("{result}");
    Ok(Report {
        cell,
        outcome,
        log_path,
    })
}

/// Runs and summarizes every requested QEMU combination.
///
/// A single run preserves its specific exit code, while vector runs finish all cells before failing.
pub fn run(root: &Path, args: &QemuArgs) -> TaskResult<u8> {
    let cells = args.cells();
    let multiple = cells.len() > 1;
    let mut reports = Vec::new();
    for (index, cell) in cells.into_iter().enumerate() {
        if multiple {
            println!("\n=== Cell {} ===", index + 1);
        }
        reports.push(run_one(
            root,
            cell,
            Duration::from_secs(args.timeout.get()),
            &args.qemu_args,
        )?);
    }
    if multiple {
        println!("\nQEMU result table");
        println!(
            "{:<3} {:<24} {:<32} {:<8} {:<5} {:<14} {:<6} Log",
            "#", "Crate", "Target", "Profile", "CPUs", "Result", "Status"
        );
        for (index, report) in reports.iter().enumerate() {
            println!(
                "{:<3} {:<24} {:<32} {:<8} {:<5} {:<14} {:<6} {}",
                index + 1,
                report.cell.build.crate_path.display(),
                report.cell.build.target.triple(),
                report.cell.build.profile.directory(),
                report.cell.cpus,
                report.outcome.label(),
                report.outcome.code(),
                report.log_path.display()
            );
        }
        Ok(u8::from(
            reports.iter().any(|report| report.outcome != Outcome::Pass),
        ))
    } else {
        Ok(reports[0].outcome.code())
    }
}
