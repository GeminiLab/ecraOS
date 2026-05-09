//! x86_64 Multiboot 1 platform support: COM1 debug console, [`explat`] trait
//! implementations, and ACPI-style shutdown for QEMU.

#![no_std]

/// UART 16550 COM1 implementation of [`explat::debug_console::DebugConsoleIf`].
pub mod debug_console;
/// [`explat::init::InitIf`] bridge into this crate's early setup.
pub mod init;
/// [`explat::power::PowerIf`] using the QEMU `0x604` PM port.
pub mod power;

/// Runs platform-local early initialization (currently COM1) before portable `InitIf` work.
pub fn init_early() {
    debug_console::init();
}
