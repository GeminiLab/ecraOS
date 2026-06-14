/// UART 16550 COM1 implementation of [`exarch::debug_console::DebugConsoleIf`].
pub mod debug_console;
/// [`exarch::init::InitIf`] bridge into this crate's early setup.
pub mod init;
/// [`exarch::mem::MemIf`] using the Multiboot 1 memory map.
pub mod mem;
/// [`exarch::power::PowerIf`] using the QEMU `0x604` PM port.
pub mod power;

/// Runs platform-local early initialization (currently COM1) before portable `InitIf` work.
pub fn init_early() {
    debug_console::init();
}
