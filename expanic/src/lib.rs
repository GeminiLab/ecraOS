//! Kernel panics and bug reports.
//!
//! Use [`kpanic!`] to report a fatal kernel error, or [`bug!`] to report an unexpected internal
//! error. Both call [`terminate`] with a formatted message and the module path, file, line, and
//! column of the macro call. [`UnwrapOrBug`] adds methods to [`Option`] and [`Result`] that report
//! missing values and errors as bugs.
//!
//! By default, [`terminate`] calls [`core::panic!`] with the supplied information. An application
//! can replace this function by applying `#[expanic::terminate_handler]` to a function with the
//! same signature. A kernel can use its replacement to log the error and shut down.
//!
//! This crate is `no_std` and requires nightly Rust for `extern_item_impls`. It does not define a
//! panic handler or provide platform shutdown code.

#![no_std]
#![feature(extern_item_impls)]

use core::{fmt, panic::Location};

mod bug;

pub use bug::UnwrapOrBug;

/// Reports a fatal kernel error and terminates execution.
///
/// Accepts a format string and optional arguments. Passes the formatted message and the macro
/// call's module path, file, line, and column to [`terminate`] with [`TerminateReason::KPanic`].
#[macro_export]
macro_rules! kpanic {
    ($($arg:tt)*) => {{
        ::core::hint::cold_path();
        $crate::terminate(
            $crate::TerminateReason::KPanic,
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

/// The reason for terminating execution.
///
/// Identifies a Rust panic, a fatal kernel error, or an unexpected internal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateReason {
    /// A Rust panic received by the consumer's panic handler.
    ///
    /// Used when a panic handler forwards a Rust panic to the application's termination function.
    Panic,
    /// A fatal kernel error reported with [`kpanic!`].
    ///
    /// Indicates that execution cannot continue because of a kernel error.
    KPanic,
    /// An unexpected internal error.
    ///
    /// Used by [`bug!`] and the methods in [`UnwrapOrBug`].
    Bug,
}

impl TerminateReason {
    /// Returns the short label for the termination reason.
    ///
    /// Returns `"Panic"`, `"Kernel panic"`, or `"Bug"` for display in an error message.
    pub fn name(self) -> &'static str {
        match self {
            TerminateReason::Panic => "Panic",
            TerminateReason::KPanic => "Kernel panic",
            TerminateReason::Bug => "Bug",
        }
    }

    /// Returns an explanation of the termination reason.
    ///
    /// Provides additional text for an error report, including a request to report internal bugs.
    pub fn desc(self) -> &'static str {
        match self {
            TerminateReason::Panic => {
                "\"Panic\" means some code called the `panic!` macro, usually from an external crate."
            }
            TerminateReason::KPanic => {
                "\"Kernel panic\" means some code called the `kpanic!` macro, usually from the kernel itself or its components."
            }
            TerminateReason::Bug => {
                "\"Bug\" means an unexpected internal error happened in the kernel or its components. Please report this issue to the maintainers."
            }
        }
    }
}

/// The source location of a panic or bug.
///
/// Stores a file, line, column, and optional module path for an error report. Macros can supply
/// all four fields. Rust's [`Location`] provides the file, line, and column, but no module path.
pub struct TerminateLocation<'a> {
    /// The caller's module path, when available.
    ///
    /// Contains `None` when constructed from a Rust panic location.
    module_path: Option<&'a str>,
    /// The caller's source file.
    ///
    /// Contains `<unknown>` when constructed without a Rust panic location.
    file: &'a str,
    /// The caller's source line.
    ///
    /// Contains zero when constructed without a Rust panic location.
    line: u32,
    /// The caller's source column.
    ///
    /// Contains zero when constructed without a Rust panic location.
    column: u32,
}

impl<'a> TerminateLocation<'a> {
    /// Creates a source location from an optional Rust panic location.
    ///
    /// Copies the file, line, and column from [`Location`] and leaves the module path as `None`.
    /// If no location is supplied, uses `<unknown>` for the file and zero for the line and column.
    pub fn from_panic_location(location: Option<&Location<'a>>) -> Self {
        if let Some(location) = location {
            Self::from_raw_parts(None, location.file(), location.line(), location.column())
        } else {
            Self::from_raw_parts(None, "<unknown>", 0, 0)
        }
    }

    /// Creates a source location from its individual fields.
    ///
    /// Stores the supplied module path, file, line, and column without changing them.
    pub fn from_raw_parts(
        module_path: Option<&'a str>,
        file: &'a str,
        line: u32,
        column: u32,
    ) -> Self {
        Self {
            module_path,
            file,
            line,
            column,
        }
    }

    /// Returns the caller's module path when available.
    ///
    /// Returns `None` when constructed from a Rust panic location.
    pub fn module_path(&self) -> Option<&'a str> {
        self.module_path
    }

    /// Returns the caller's source file.
    ///
    /// Returns `<unknown>` when constructed without a Rust panic location.
    pub fn file(&self) -> &'a str {
        self.file
    }

    /// Returns the caller's source line.
    ///
    /// Returns zero when constructed without a Rust panic location.
    pub fn line(&self) -> u32 {
        self.line
    }

    /// Returns the caller's source column.
    ///
    /// Returns zero when constructed without a Rust panic location.
    pub fn column(&self) -> u32 {
        self.column
    }
}

/// Terminates execution with a reason, source location, and message.
///
/// The default implementation calls [`core::panic!`] with the supplied information in its message.
/// Whether this unwinds or aborts depends on the application's panic strategy and panic handler.
///
/// To change this behavior, apply `#[expanic::terminate_handler]` to a function with the same
/// signature. A kernel can forward ordinary Rust panics to that function from its panic handler.
/// Calling the default implementation from a panic handler would cause another panic.
#[eii(terminate_handler)]
pub fn terminate(
    reason: TerminateReason,
    location: TerminateLocation<'_>,
    args: fmt::Arguments<'_>,
) -> ! {
    terminate_fallback(reason, location, args)
}

/// Keeps the default termination symbol available to optimized standalone consumers.
///
/// EII normally emits the default implementation only when the defining crate references its
/// public symbol. This reference preserves that weak symbol for consumers that use the fallback.
#[used]
static DEFAULT_TERMINATE: fn(
    TerminateReason,
    TerminateLocation<'static>,
    fmt::Arguments<'static>,
) -> ! = terminate;

/// Panics with the supplied reason, source location, and message.
///
/// Includes the original location in the panic message so it remains available to a panic handler.
#[inline(never)]
fn terminate_fallback(
    reason: TerminateReason,
    location: TerminateLocation<'_>,
    args: fmt::Arguments<'_>,
) -> ! {
    core::panic!(
        "[{}:{}] {} at {}:{}:{}:\n{}\n\n{}",
        location.module_path().unwrap_or("<unknown module>"),
        location.line(),
        reason.name(),
        location.file(),
        location.line(),
        location.column(),
        args,
        reason.desc(),
    )
}
