//! Process orchestration regression tests.
//!
//! Real Cargo metadata is combined with small executable fixtures so failure and timeout paths
//! can be tested without booting a kernel. Real QEMU smoke runs verify the complete build later.

#![cfg(unix)]

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// A counter distinguishing concurrent executable fixtures.
///
/// Combines with the process ID and timestamp to avoid temporary-directory collisions.
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// An isolated external tool fixture.
///
/// Only Cargo metadata uses the real toolchain, while builds, stripping, and guests are controlled.
struct Fixture {
    /// The fixture's temporary directory.
    ///
    /// Includes a space to exercise subprocess path handling.
    dir: PathBuf,
}

impl Fixture {
    /// Creates the command fixtures.
    ///
    /// The guest reports a fatal error on CPU count 1, succeeds on other counts, or sleeps on demand.
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("ecra xtask-{}-{stamp}-{id}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        executable(
            &dir.join("cargo"),
            r#"#!/bin/sh
if [ "$1" = metadata ]; then exec "$REAL_CARGO" "$@"; fi
if [ "$FAIL_BUILD" = 1 ]; then echo 'fixture build failure' >&2; exit 9; fi
printf '%s\n' "$CARGO_ENCODED_RUSTFLAGS" >> "$FLAG_RECORD"
exit 0
"#,
        );
        executable(
            &dir.join("rust-objcopy"),
            r#"#!/bin/sh
mkdir -p "$(dirname "$1")"
printf image > "$1"
for last do :; done
printf image > "$last"
"#,
        );
        executable(
            &dir.join("qemu-system-x86_64"),
            r#"#!/bin/sh
printf '%s\n' "$$" > "$PID_RECORD"
if [ "$HANG_GUEST" = 1 ]; then exec sleep 60; fi
if [ "$READ_GUEST_INPUT" = 1 ]; then
    IFS= read -r input
    printf 'guest input: %s\n' "$input"
    exit 0
fi
while [ "$#" -gt 0 ]; do
    if [ "$1" = -smp ]; then shift; cpus="$1"; fi
    shift
done
printf '%s\n' "$cpus" >> "$CPU_RECORD"
if [ "$cpus" = 1 ]; then echo 'Kernel panic: fixture failure' >&2; else echo 'fixture success'; fi
exit 0
"#,
        );
        Self { dir }
    }

    /// Prepares an xtask process with isolated tool and artifact paths.
    ///
    /// Each child receives environment options without mutating the test process environment.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        for name in ["CRATE", "TARGET", "PROFILE", "CPUS", "TIMEOUT_SEC", "CLEAN"] {
            command.env_remove(name);
        }
        let mut paths = vec![self.dir.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .current_dir(&self.dir)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("REAL_CARGO", env!("CARGO"))
            .env("CARGO", self.dir.join("cargo"))
            .env("CARGO_TARGET_DIR", self.dir.join("artifacts with spaces"))
            .env("CPU_RECORD", self.dir.join("cpus"))
            .env("PID_RECORD", self.dir.join("pid"))
            .env("FLAG_RECORD", self.dir.join("flags"));
        command
    }
}

impl Drop for Fixture {
    /// Removes the isolated fixture after its processes have completed.
    ///
    /// Production repository artifacts and QEMU logs are not removed here.
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

/// Writes an executable fixture script.
///
/// A fixture models an external process contract rather than Rust implementation internals.
fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Explains subprocess output in assertion failures.
///
/// Captures both streams because build diagnostics and the result table use different channels.
fn diagnostic(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Runs every vector cell even when an earlier guest fails.
///
/// A fatal log must defeat a zero guest exit status without stopping subsequent CPU variants.
#[test]
fn continues_after_a_guest_fails() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["qemu", "--cpus", "1,4"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", diagnostic(&output));
    assert_eq!(
        fs::read_to_string(fixture.dir.join("cpus")).unwrap(),
        "1\n4\n"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FAIL") && stdout.contains("PASS"),
        "{}",
        diagnostic(&output)
    );
}

/// Reports build failures separately from guest failures.
///
/// No guest may be started after the selected application fails to build.
#[test]
fn reports_build_failure_without_starting_qemu() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FAIL_BUILD", "1")
        .arg("qemu")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("BUILD_FAILURE"),
        "{}",
        diagnostic(&output)
    );
    assert!(!fixture.dir.join("pid").exists());
}

/// Terminates and reaps a guest when its timeout expires.
///
/// A killed process must not keep pipes open or remain alive after xtask returns.
#[test]
fn kills_a_timed_out_guest() {
    let fixture = Fixture::new();
    let start = Instant::now();
    let output = fixture
        .command()
        .env("HANG_GUEST", "1")
        .args(["qemu", "--timeout", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(124), "{}", diagnostic(&output));
    assert!(
        start.elapsed().as_secs() < 10,
        "timeout failed to close guest pipes"
    );
    let pid = fs::read_to_string(fixture.dir.join("pid")).unwrap();
    assert!(
        !Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

/// Prints only the loader path on build stdout.
///
/// An absolute application path and a target directory containing spaces must work from any cwd.
#[test]
fn builds_with_separate_manifest_and_artifact_paths() {
    let fixture = Fixture::new();
    let crate_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("smoke-apps/default");
    let output = fixture
        .command()
        .args(["build", "--clean", "--crate"])
        .arg(crate_path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostic(&output));
    let loader = fixture
        .dir
        .join("artifacts with spaces/x86_64-unknown-none/debug/ecraldr-x86_64-multiboot");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        loader.to_str().unwrap()
    );
    assert!(loader.is_file());
    let flags = fs::read_to_string(fixture.dir.join("flags")).unwrap();
    assert!(flags.contains("relocation-model=pie"));
    assert!(flags.contains("ecraos/ecraos/link.ld"));
    assert!(flags.contains("relocation-model=static"));
}

/// Prioritizes explicit scalar options over environment vectors.
///
/// Environment defaults must not add unexpected CPU combinations to a requested CI cell.
#[test]
fn explicit_cpu_option_overrides_environment_vector() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("CPUS", "1,2")
        .args(["qemu", "--cpus", "4"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostic(&output));
    assert_eq!(fs::read_to_string(fixture.dir.join("cpus")).unwrap(), "4\n");
}

/// Forwards console input to an interactive QEMU process.
///
/// The migrated runner must preserve the former script's inherited guest stdin.
#[test]
fn preserves_guest_console_input() {
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .env("READ_GUEST_INPUT", "1")
        .arg("qemu")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"console input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", diagnostic(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("guest input: console input"),
        "{}",
        diagnostic(&output)
    );
}

/// Preserves the active guest log when cleaning shared Cargo artifacts.
///
/// Only the selected guest target is cleared, retaining sibling logs and host artifacts.
#[test]
fn clean_keeps_guest_log_available() {
    let fixture = Fixture::new();
    let artifacts = fixture.dir.join("artifacts with spaces");
    let stale = artifacts.join("x86_64-unknown-none/debug/stale.bin");
    let retained_log = artifacts.join("qemu-logs/retained.log");
    let retained_host = artifacts.join("debug/retained-host-file");
    for path in [&stale, &retained_log, &retained_host] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "sentinel").unwrap();
    }
    let output = fixture
        .command()
        .args(["qemu", "--clean", "--cpus", "4"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostic(&output));
    assert_eq!(fs::read_to_string(fixture.dir.join("cpus")).unwrap(), "4\n");
    assert!(!stale.exists(), "clean retained stale guest artifacts");
    assert_eq!(fs::read_to_string(retained_log).unwrap(), "sentinel");
    assert_eq!(fs::read_to_string(retained_host).unwrap(), "sentinel");
}
