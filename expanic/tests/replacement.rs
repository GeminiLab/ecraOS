//! Consumer-provided termination backend.
//!
//! Exercises the cross-crate EII boundary with the original caller metadata and borrowed message.

use core::fmt;

use expanic::{TerminateLocation, TerminateReason, UnwrapOrBug};

/// The terminal context observed by the consumer backend.
///
/// Owns the borrowed fields so the test can inspect them after unwinding.
#[derive(Debug)]
struct ObservedTermination {
    /// The original termination reason.
    ///
    /// Distinguishes the explicit bug path from an ordinary panic fallback.
    reason: TerminateReason,
    /// The caller's module path.
    ///
    /// Retains the macro expansion context in the consumer crate.
    module_path: Option<String>,
    /// The caller's source file.
    ///
    /// Retains the file reported by the macro expansion.
    file: String,
    /// The caller's source line.
    ///
    /// Identifies the exact invocation rather than the fallback function.
    line: u32,
    /// The caller's source column.
    ///
    /// Identifies the beginning of the macro invocation.
    column: u32,
    /// The rendered borrowed message.
    ///
    /// Confirms formatting arguments survive the EII boundary.
    message: String,
}

/// Captures terminal diagnostics using the consumer's own exit policy.
///
/// Replaces expanic's default panic exit through its public EII attribute.
#[expanic::terminate_handler]
fn terminate(
    reason: TerminateReason,
    location: TerminateLocation<'_>,
    args: fmt::Arguments<'_>,
) -> ! {
    std::panic::panic_any(ObservedTermination {
        reason,
        module_path: location.module_path().map(str::to_owned),
        file: location.file().to_owned(),
        line: location.line(),
        column: location.column(),
        message: args.to_string(),
    })
}

/// Verifies dispatch and original diagnostics across the reusable crate boundary.
///
/// Detects accidental fallback dispatch or a loss of borrowed arguments and caller metadata.
#[test]
fn replacement_receives_original_context() {
    let value = String::from("borrowed message");
    let line = line!() + 2;
    let payload = std::panic::catch_unwind(|| {
        expanic::bug!("failure: {}", value);
    })
    .expect_err("termination must not return");
    let observed = payload
        .downcast::<ObservedTermination>()
        .expect("EII must dispatch to the consumer backend");
    assert_eq!(observed.reason, TerminateReason::Bug);
    assert_eq!(observed.module_path.as_deref(), Some(module_path!()));
    assert_eq!(observed.file, file!());
    assert_eq!(observed.line, line);
    assert_eq!(observed.column, 9);
    assert_eq!(observed.message, "failure: borrowed message");
}

/// Verifies failed extension methods pass the callsite and borrowed error to the backend.
///
/// Detects methods bypassing the EII exit or inventing a module path from their own definition.
#[test]
fn unwrap_replacement_receives_original_context() {
    let error = String::from("borrowed error");
    let line = line!() + 2;
    let payload = std::panic::catch_unwind(|| {
        Err::<u8, _>(&error).unwrap_or_bug();
    })
    .expect_err("failed unwrap must terminate");
    let observed = payload
        .downcast::<ObservedTermination>()
        .expect("failed unwrap must reach the consumer backend");
    assert_eq!(observed.reason, TerminateReason::Bug);
    assert_eq!(observed.module_path, None);
    assert_eq!(observed.file, file!());
    assert_eq!(observed.line, line);
    assert_eq!(observed.column, 30);
    assert!(observed.message.contains(r#""borrowed error""#));
}

/// Verifies custom failure context and the callsite survive replacement dispatch.
///
/// Detects `expect_or_bug` losing borrowed context or reporting its implementation's location.
#[test]
fn expect_replacement_receives_original_context() {
    let context = String::from("borrowed context");
    let line = line!() + 2;
    let payload = std::panic::catch_unwind(|| {
        None::<u8>.expect_or_bug(&context);
    })
    .expect_err("failed expect must terminate");
    let observed = payload
        .downcast::<ObservedTermination>()
        .expect("failed expect must reach the consumer backend");
    assert_eq!(observed.reason, TerminateReason::Bug);
    assert_eq!(observed.module_path, None);
    assert_eq!(observed.file, file!());
    assert_eq!(observed.line, line);
    assert_eq!(observed.column, 20);
    assert_eq!(observed.message, "borrowed context: ()");
}
