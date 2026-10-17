//! Kernel panic handling and terminal diagnostics.
//!
//! Supplies the expanic backend for kernel logging and shutdown, with a host-safe test exit.

use core::fmt;

#[cfg(not(feature = "host-test"))]
use exarch::power::{ShutdownReason, shutdown};
pub use expanic::*;
#[cfg(not(feature = "host-test"))]
use log::Record;

#[cfg(not(feature = "host-test"))]
use crate::kprintln;

/// Logs a terminal diagnostic and stops kernel execution.
///
/// Host tests unwind with the diagnostic as a panic payload. Kernel builds shut down the platform.
#[expanic::terminate_handler]
#[inline(never)]
pub fn terminate(
    reason: TerminateReason,
    location: TerminateLocation<'_>,
    args: fmt::Arguments<'_>,
) -> ! {
    let full_arg = format_args!(
        "{} at {}:{}:{}:\n{}\n\n{}",
        reason.name(),
        location.file(),
        location.line(),
        location.column(),
        args,
        reason.desc()
    );

    #[cfg(feature = "host-test")]
    {
        // Use `core::panic!` as the fallback for host tests.

        core::panic!(
            "[{}:{}] {}",
            location.module_path().unwrap_or("<unknown module>"),
            location.line(),
            full_arg,
        )
    }

    #[cfg(not(feature = "host-test"))]
    {
        if crate::logging::is_inited() {
            let record = Record::builder()
                .level(log::Level::Error)
                .target(location.module_path().unwrap_or("<unknown module>"))
                .file(Some(location.file()))
                .line(Some(location.line()))
                .args(full_arg)
                .build();

            log::logger().log(&record);
        } else {
            kprintln!("{}", full_arg);
        }

        shutdown(ShutdownReason::Panicked)
    }
}

/// Routes Rust panics through the shared termination path.
///
/// Calls the local backend directly so an absent EII replacement cannot recurse through the
/// default panic exit. Records the panic location when available and shuts down the kernel.
#[cfg(not(feature = "host-test"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    terminate(
        TerminateReason::Panic,
        TerminateLocation::from_panic_location(info.location()),
        format_args!("{}", info.message()),
    )
}

/// Host termination regression tests.
///
/// Runs termination in a bounded subprocess so hardware access cannot kill the test harness.
#[cfg(test)]
mod tests {
    use std::{
        panic::{UnwindSafe, catch_unwind},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use expanic::UnwrapOrBug;

    use super::{TerminateLocation, TerminateReason, terminate};

    /// Verifies that a terminal call unwinds with the supplied diagnostic context.
    ///
    /// Checks the actual panic payload rather than relying on the default host panic hook.
    fn assert_termination(call: impl FnOnce() + UnwindSafe, expected: &[&str]) {
        let payload = catch_unwind(call).expect_err("termination must not return");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .expect("termination must preserve a readable diagnostic");
        for fragment in expected {
            assert!(
                message.contains(fragment),
                "missing {fragment:?} in {message:?}"
            );
        }
    }

    /// Verifies that host termination unwinds without using kernel devices.
    ///
    /// Isolates privileged instructions and bounds regressions that would otherwise hang the suite.
    #[test]
    fn host_termination_unwinds_with_diagnostics() {
        /// The environment flag selecting the isolated termination probe.
        ///
        /// Prevents the child test from launching another subprocess.
        const CHILD_FLAG: &str = "ECRAOS_HOST_TERMINATION_PROBE";
        if std::env::var_os(CHILD_FLAG).is_some() {
            std::panic::set_hook(Box::new(|_| {}));
            assert_termination(
                || crate::bug!("failure {}", 42),
                &[module_path!(), file!(), "failure 42"],
            );
            assert_termination(
                || crate::kpanic!("kernel failure {}", 42),
                &[
                    module_path!(),
                    file!(),
                    "Kernel panic at ",
                    "kernel failure 42",
                ],
            );
            assert_termination(
                || expanic::bug!("component failure {}", 42),
                &[module_path!(), file!(), "Bug at ", "component failure 42"],
            );
            assert_termination(
                || {
                    None::<u8>.unwrap_or_bug();
                },
                &[file!(), "Bug at ", "unexpected value: ()"],
            );
            assert_termination(
                || {
                    Err::<u8, _>("missing component").unwrap_or_bug();
                },
                &[file!(), "Bug at ", "missing component"],
            );
            assert_termination(
                || {
                    None::<u8>.expect_or_bug("required component missing");
                },
                &[file!(), "Bug at ", "required component missing: ()"],
            );
            assert_termination(
                || {
                    Err::<u8, _>("component error").expect_or_bug("initialization failed");
                },
                &[
                    file!(),
                    "Bug at ",
                    "initialization failed",
                    "component error",
                ],
            );
            assert_termination(
                || expanic::kpanic!("component panic {}", 42),
                &[
                    module_path!(),
                    file!(),
                    "Kernel panic at ",
                    "component panic 42",
                ],
            );
            assert_termination(
                || {
                    terminate(
                        TerminateReason::Panic,
                        TerminateLocation::from_panic_location(None),
                        format_args!("missing location"),
                    );
                },
                &[
                    "<unknown module>",
                    "Panic at <unknown>:0:0:",
                    "missing location",
                ],
            );
            return;
        }

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "panic::tests::host_termination_unwinds_with_diagnostics",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start termination probe");
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("host termination probe hung");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "termination probe failed ({}):\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
