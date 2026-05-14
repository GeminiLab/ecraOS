#![no_std]

pub use memory_addr::{PhysAddr, PhysAddrRange};

pub use exboot_macros::*;

/// The argument provided by the bootloader or firmware.
///
/// This is the second argument passed to the kernel entry function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformArg {
    /// The Multiboot information structure.
    Multiboot(PhysAddr),
    /// No argument provided.
    None,
}

/// The argument provided by the `exboot`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExbootArg {
    pub boot_stack: PhysAddrRange,
    pub plat_arg: PlatformArg,
}

/// The expected signature of the kernel entry function, i.e. the function that
/// is marked with [`kernel_entry`].
pub type KernelEntryType = extern "Rust" fn(hart_id: usize, arg: *const ExbootArg) -> !;
