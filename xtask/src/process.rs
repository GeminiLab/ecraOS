//! External command execution.
//!
//! Routes build diagnostics and guarantees that spawned children are reaped on errors and timeouts.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::TaskResult;

/// A child process owned by a running task.
///
/// Dropping the owner terminates and reaps an unfinished child.
struct ChildOwner(Child);

impl Drop for ChildOwner {
    /// Terminates an unfinished process and collects its status.
    ///
    /// Already completed children need no additional termination signal.
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Prepares Cargo in the repository root.
///
/// Honors Cargo's executable environment setting while keeping manifest paths explicit.
pub fn cargo(root: &Path) -> Command {
    let executable = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(executable);
    command.current_dir(root);
    command
}

/// Runs a build or host command and checks its exit status.
///
/// Child output goes to the supplied log or stderr, preserving build stdout for the artifact path.
pub fn run(command: &mut Command, log: Option<&File>) -> TaskResult<()> {
    let description = format!("{command:?}");
    command.stdin(Stdio::null());
    if let Some(mut log) = log {
        writeln!(log, "Running {description}")?;
        command.stdout(log.try_clone()?).stderr(log.try_clone()?);
    } else {
        eprintln!("Running {description}");
        command.stdout(Stdio::piped()).stderr(Stdio::inherit());
    }
    let mut child = ChildOwner(command.spawn()?);
    if let Some(mut stdout) = child.0.stdout.take() {
        io::copy(&mut stdout, &mut io::stderr().lock())?;
    }
    let status = child.0.wait()?;
    if !status.success() {
        return Err(format!("command failed ({status}): {description}").into());
    }
    Ok(())
}

/// Runs a guest with byte-preserving output and a runtime limit.
///
/// Both guest streams append to one file. A separate reader forwards new bytes to stdout without
/// pipes or reader threads that could prevent timeout termination. Guest console input is inherited.
pub fn guest(
    command: &mut Command,
    log: &File,
    log_path: &Path,
    timeout: Duration,
) -> TaskResult<(ExitStatus, bool, Vec<u8>)> {
    let mut reader = File::open(log_path)?;
    let guest_start = reader.seek(SeekFrom::End(0))?;
    command
        .stdin(Stdio::inherit())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
    let mut child = ChildOwner(command.spawn()?);
    let started = Instant::now();
    let mut buffer = [0; 16384];
    let mut terminal = io::stdout().lock();
    let (status, timed_out) = loop {
        let count = reader.read(&mut buffer)?;
        terminal.write_all(&buffer[..count])?;
        terminal.flush()?;
        if let Some(status) = child.0.try_wait()? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            if let Err(error) = child.0.kill()
                && child.0.try_wait()?.is_none()
            {
                return Err(error.into());
            }
            break (child.0.wait()?, true);
        }
        if count == 0 {
            thread::sleep(Duration::from_millis(10));
        }
    };
    io::copy(&mut reader, &mut terminal)?;
    terminal.flush()?;
    reader.seek(SeekFrom::Start(guest_start))?;
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    Ok((status, timed_out, output))
}
