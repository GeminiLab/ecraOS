//! Debug console abstraction: write and read bytes through a platform-selected
//! debug console device.

use core::fmt::Write;

use crate::crate_interface::def_interface;

/// Low-level debug console operations implemented by the platform.
///
/// Methods in this trait may be unavailable before [`init_early`](crate::init::InitIf::init_early)
/// is called.
#[def_interface(gen_caller)]
pub trait DebugConsoleIf {
    /// Writes every byte in `bytes` to the console device.
    fn write_bytes(bytes: &[u8]);
    /// Reads as many bytes as are immediately available, storing them in `bytes`.
    ///
    /// Returns how many bytes were read (may be less than `bytes.len()` if the input is idle).
    fn read_bytes(bytes: &mut [u8]) -> usize;
}

/// Writes a UTF-8 string to the debug console.
pub fn write_str<S: AsRef<str>>(s: S) {
    let s = s.as_ref();
    let b = s.as_bytes();
    write_bytes(b);
}

/// [`core::fmt::Write`] adapter that forwards to [`write_str`].
pub struct DebugConsole;

impl Write for DebugConsole {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_str(s);
        Ok(())
    }
}

/// Write formatted content to the debug console.
pub fn write_fmt(args: core::fmt::Arguments) {
    // SAFETY: `write_fmt` returns `Err` iff `write_str` returns `Err`, which is
    // impossible for `DebugConsole`.
    //
    // Also, panicking here is logically incorrect, because panic handler will
    // use debug console to print the panic message.
    unsafe {
        Write::write_fmt(&mut DebugConsole, args).unwrap_unchecked();
    }
}

/// Prints formatted text to the debug console (no trailing newline).
#[macro_export]
macro_rules! dbcn_print {
    ($($arg:tt)*) => {
        $crate::debug_console::write_fmt(format_args!($($arg)*))
    };
}

/// Prints formatted text to the debug console, followed by a newline.
#[macro_export]
macro_rules! dbcn_println {
    () => {
        $crate::debug_console::write_str("\n")
    };
    ($($arg:tt)*) => {
        $crate::debug_console::write_fmt(
            format_args!("{}\n", format_args!($($arg)*))
        )
    };
}
