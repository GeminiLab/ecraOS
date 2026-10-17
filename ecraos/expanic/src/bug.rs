//! Bug reports and methods for unwrapping options and results.
//!
//! Reports unexpected internal errors through `bug!` and the methods in [`UnwrapOrBug`].

use core::{fmt::Debug, panic::Location};

use crate::{TerminateLocation, TerminateReason, terminate};

/// Reports an unexpected internal error and terminates execution.
///
/// Use this when an internal assumption is violated. Accepts a format string and optional
/// arguments, then passes the formatted message and the macro call's module path, file, line, and
/// column to [`terminate`] with [`TerminateReason::Bug`].
#[macro_export]
macro_rules! bug {
    ($($arg:tt)*) => {{
        ::core::hint::cold_path();
        $crate::terminate(
            $crate::TerminateReason::Bug,
            $crate::TerminateLocation::from_raw_parts(
                Some(module_path!()),
                file!(),
                line!(),
                column!(),
            ),
            format_args!($($arg)*),
        )
    }};
}

/// Methods that unwrap an [`Option`] or [`Result`] and report failures as bugs.
///
/// Returns the value in `Some` or `Ok`. On `None` or `Err`, calls [`terminate`] with
/// [`TerminateReason::Bug`], an error message, and the file, line, and column of the method call.
/// The caller's module path is unavailable for these method calls.
///
/// ```
/// use expanic::UnwrapOrBug;
///
/// assert_eq!(Some(42).unwrap_or_bug(), 42);
/// assert_eq!(Ok::<_, &str>(42).expect_or_bug("initialization failed"), 42);
/// ```
pub trait UnwrapOrBug: Sized {
    /// The value returned on success.
    ///
    /// This is `T` for both `Option<T>` and `Result<T, E>`.
    type Output;
    /// The error included in the bug report.
    ///
    /// This is `E` for `Result<T, E>`, or `()` for `Option<T>`. It is formatted using [`Debug`].
    type Err: Debug;

    /// Converts the value to a result.
    ///
    /// Returns a `Result` unchanged, or converts an `Option` to `Ok(value)` or `Err(())`.
    #[doc(hidden)]
    fn to_result(self) -> Result<Self::Output, Self::Err>;

    /// Returns the value in `Some` or `Ok`, or reports a bug and terminates execution.
    ///
    /// On `None` or `Err`, calls [`terminate`] with [`TerminateReason::Bug`] and the method call's
    /// source location. The message includes the error formatted using [`Debug`], or `()` for
    /// `None`.
    #[track_caller]
    fn unwrap_or_bug(self) -> Self::Output {
        match self.to_result() {
            Ok(value) => value,
            Err(e) => {
                core::hint::cold_path();
                terminate(
                    TerminateReason::Bug,
                    TerminateLocation::from_panic_location(Some(Location::caller())),
                    format_args!("unexpected value: {:?}", e),
                )
            }
        }
    }

    /// Returns the value in `Some` or `Ok`, or reports a bug with the supplied message.
    ///
    /// On `None` or `Err`, terminates execution as [`Self::unwrap_or_bug`] does, adding `msg`
    /// before the error in the report. Includes the source location of this method call.
    #[track_caller]
    fn expect_or_bug(self, msg: &str) -> Self::Output {
        match self.to_result() {
            Ok(value) => value,
            Err(e) => {
                core::hint::cold_path();
                terminate(
                    TerminateReason::Bug,
                    TerminateLocation::from_panic_location(Some(Location::caller())),
                    format_args!("{}: {:?}", msg, e),
                )
            }
        }
    }
}

impl<T> UnwrapOrBug for Option<T> {
    /// The contained value.
    ///
    /// Returned when the option is `Some`.
    type Output = T;
    /// The error reported for `None`.
    ///
    /// Uses `()` because `None` contains no error value.
    type Err = ();

    /// Converts the option to a result.
    ///
    /// Returns `Ok(value)` for `Some(value)`, or `Err(())` for `None`.
    fn to_result(self) -> Result<Self::Output, Self::Err> {
        self.ok_or(())
    }
}

impl<T, E: Debug> UnwrapOrBug for Result<T, E> {
    /// The successful result value.
    ///
    /// Returned when the result is `Ok`.
    type Output = T;
    /// The original result error.
    ///
    /// Included in the bug report using [`Debug`].
    type Err = E;

    /// Returns the result without changing either variant.
    ///
    /// Preserves the success or error value.
    fn to_result(self) -> Result<T, E> {
        self
    }
}
