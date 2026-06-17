pub mod debug_console;
pub mod imp;
pub mod init;
pub mod mem;
pub mod power;
pub mod reloc_hook;

/// Runs platform-local early initialization (currently COM1) before portable `InitIf` work.
pub fn init_early() {
    imp::gdt::init_gdt();
    imp::idt::init_idt();
    debug_console::init();
}

pub fn after_reloc() {
    imp::gdt::reload_gdt();
    imp::idt::reload_idt();
}

// TODO: remove this
pub use imp::TrapFrame;
