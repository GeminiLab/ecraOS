//! Result and option bug handling.
//!
//! Exercises the public extension trait from a standalone consumer crate.

use std::panic::catch_unwind;

use expanic::UnwrapOrBug;

/// Converts a termination payload into its formatted diagnostic.
///
/// Keeps assertions independent of the standard panic hook's output formatting.
fn diagnostic(payload: Box<dyn std::any::Any + Send>) -> String {
    *payload
        .downcast::<String>()
        .expect("bug termination must preserve a formatted diagnostic")
}

/// Verifies successful option and result values pass through unchanged.
///
/// Detects implementations that accidentally terminate on the success branch.
#[test]
fn successful_values_are_returned() {
    assert_eq!(Some(17).unwrap_or_bug(), 17);
    assert_eq!(Some(19).expect_or_bug("option value missing"), 19);
    assert_eq!(Ok::<_, &str>(23).unwrap_or_bug(), 23);
    assert_eq!(Ok::<_, &str>(29).expect_or_bug("result failed"), 29);
}

/// Verifies successful values need neither copying nor debug formatting.
///
/// Detects accidental bounds on the output type or an early drop of an owned value.
#[test]
fn successful_values_preserve_owned_outputs_without_debug() {
    /// An owned output without `Copy`, `Clone`, or `Debug`.
    ///
    /// Ensures only the error type needs debug formatting.
    struct Value(String);

    assert_eq!(
        Some(Value(String::from("option"))).unwrap_or_bug().0,
        "option"
    );
    assert_eq!(
        Some(Value(String::from("expected option")))
            .expect_or_bug("missing")
            .0,
        "expected option",
    );
    assert_eq!(
        Ok::<_, ()>(Value(String::from("result"))).unwrap_or_bug().0,
        "result",
    );
    assert_eq!(
        Ok::<_, ()>(Value(String::from("expected result")))
            .expect_or_bug("failed")
            .0,
        "expected result",
    );
}

/// Verifies a missing option terminates with the caller's source location.
///
/// Detects `track_caller` being lost before the bug backend receives the failure.
#[test]
fn missing_option_reports_the_caller_location() {
    let caller_line = line!() + 2;
    let payload =
        catch_unwind(|| None::<u8>.unwrap_or_bug()).expect_err("None must terminate as a bug");
    let message = diagnostic(payload);
    assert!(message.contains("Bug at "), "{message}");
    assert!(
        message.contains(&format!("{}:{caller_line}:", file!())),
        "missing callsite {}:{caller_line} in {message:?}",
        file!(),
    );
    assert!(message.contains("unexpected value: ()"), "{message}");
}

/// Verifies custom option context reaches the bug backend at the caller's location.
///
/// Detects an option failure losing the supplied message or using the trait definition's file.
#[test]
fn missing_option_reports_custom_context_and_caller_location() {
    let caller_line = line!() + 2;
    let payload = catch_unwind(|| {
        None::<u8>.expect_or_bug("required boot argument missing");
    })
    .expect_err("None must terminate as a bug");
    let message = diagnostic(payload);
    assert!(message.contains("Bug at "), "{message}");
    assert!(
        message.contains(&format!("{}:{caller_line}:", file!())),
        "missing callsite {}:{caller_line} in {message:?}",
        file!(),
    );
    assert!(
        message.contains("required boot argument missing: ()"),
        "{message}",
    );
}

/// Verifies a result error reaches the bug backend with its debug value and callsite.
///
/// Detects a result-specific conversion that drops the error before termination.
#[test]
fn result_error_reports_the_caller_location() {
    let caller_line = line!() + 1;
    let payload = catch_unwind(|| Err::<u8, _>("device unavailable").unwrap_or_bug())
        .expect_err("Err must terminate as a bug");
    let message = diagnostic(payload);
    assert!(message.contains("Bug at "), "{message}");
    assert!(
        message.contains(&format!("{}:{caller_line}:", file!())),
        "missing callsite {}:{caller_line} in {message:?}",
        file!(),
    );
    assert!(
        message.contains("unexpected value: \"device unavailable\""),
        "{message}"
    );
}

/// Verifies a result error and custom context reach the bug backend.
///
/// Detects losing either the `expect_or_bug` message or the original error value.
#[test]
fn result_error_reports_custom_context_and_caller_location() {
    let caller_line = line!() + 2;
    let payload =
        catch_unwind(|| Err::<u8, _>("device unavailable").expect_or_bug("initialization failed"))
            .expect_err("Err must terminate as a bug");
    let message = diagnostic(payload);
    assert!(message.contains("Bug at "), "{message}");
    assert!(
        message.contains(&format!("{}:{caller_line}:", file!())),
        "missing callsite {}:{caller_line} in {message:?}",
        file!(),
    );
    assert!(
        message.contains(r#"initialization failed: "device unavailable""#),
        "{message}"
    );
}
