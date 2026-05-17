//! `ecraOS` loader stage binary.
//!
//! This binary is the loader stage of the kernel. It is statically linked and
//! is responsible for very early boot and initialization.

#![no_std]
#![no_main]
#![feature(used_with_arg)] // Used when including the kernel.

use exboot::BOOTSTACK_SIZE;

#[unsafe(link_section = ".bss.boot_stack")]
#[used(compiler)]
#[used(linker)]
static BOOTSTACK: [u8; BOOTSTACK_SIZE] = [0; BOOTSTACK_SIZE];

// Include the kernel.
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));

#[cfg(building_ecraos_loader)]
extern crate exboot_impl;

/// Loader stage panic handler.
///
/// This handler is called when a panic occurs during the loader stage. The
/// kernel has its own panic handler.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
