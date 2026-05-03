#![no_std]

pub mod debug_console;
pub mod init;
pub mod multiboot;
pub mod power;

pub fn init_early() {
    debug_console::init();
}
