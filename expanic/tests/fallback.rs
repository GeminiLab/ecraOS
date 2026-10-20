//! Standalone termination diagnostics.
//!
//! Exercises the default exit without an ecraos dependency or an EII replacement.

use std::panic::{UnwindSafe, catch_unwind};

use expanic::{TerminateLocation, TerminateReason, terminate};

/// Captures a readable diagnostic from the default termination exit.
///
/// Ensures standalone consumers can use their normal panic unwinding policy.
fn diagnostic(call: impl FnOnce() + UnwindSafe) -> String {
    let payload = catch_unwind(call).expect_err("termination must not return");
    *payload
        .downcast::<String>()
        .expect("termination must retain a formatted diagnostic")
}

/// Verifies that bug diagnostics retain the macro caller's source and message.
///
/// Detects metadata captured inside expanic instead of at the consumer's call site.
#[test]
fn bug_preserves_caller_metadata() {
    let value = String::from("borrowed message");
    let line = line!() + 1;
    let message = diagnostic(|| expanic::bug!("failure: {}", value));
    assert!(message.contains("Bug at "), "{message}");
    assert!(message.contains(module_path!()), "{message}");
    assert!(
        message.contains(&format!("{}:{line}:", file!())),
        "{message}"
    );
    assert!(message.contains("failure: borrowed message"), "{message}");
}

/// Verifies that explicit kernel panics retain their reason and source.
///
/// Detects a macro that accidentally reports a bug or drops its caller metadata.
#[test]
fn kpanic_preserves_caller_metadata() {
    let line = line!() + 1;
    let message = diagnostic(|| expanic::kpanic!("failure {}", 42));
    assert!(message.contains("Kernel panic at "), "{message}");
    assert!(message.contains(module_path!()), "{message}");
    assert!(
        message.contains(&format!("{}:{line}:", file!())),
        "{message}"
    );
    assert!(message.contains("failure 42"), "{message}");
}

/// Verifies that missing panic metadata still produces a readable termination.
///
/// Prevents handling absent Rust panic locations from causing a secondary panic.
#[test]
fn missing_panic_location_uses_unknown_metadata() {
    let message = diagnostic(|| {
        terminate(
            TerminateReason::Panic,
            TerminateLocation::from_panic_location(None),
            format_args!("missing location"),
        )
    });
    assert!(message.contains("Panic at <unknown>:0:0:"), "{message}");
    assert!(message.contains("<unknown module>"), "{message}");
    assert!(message.contains("missing location"), "{message}");
}
