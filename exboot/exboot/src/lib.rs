#![no_std]

pub use exboot_macros::*;

/// The argument provided by the bootloader or firmware.
///
/// This is the second argument passed to the kernel entry function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootArg {
    /// The Multiboot information structure.
    Multiboot(usize),
    /// No argument provided.
    None,
}

/// The expected signature of the kernel entry function, i.e. the function that
/// is marked with [`kernel_entry`].
pub type KernelEntryType = extern "Rust" fn(hart_id: usize, arg: BootArg) -> !;
