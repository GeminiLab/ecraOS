#![no_std]

pub use memory_addr::{PhysAddr, PhysAddrRange};

pub use exboot_macros::*;

/// The argument provided by the bootloader or firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformBootArg {
    /// The Multiboot information structure.
    Multiboot(PhysAddr),
    /// No argument provided.
    None,
}

/// The argument provided by the `exboot` during boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootArg {
    pub boot_stack: PhysAddrRange,
    pub plat_arg: PlatformBootArg,
}

/// The expected signature of the kernel entry function, i.e. the function that
/// is marked with [`kernel_entry`].
pub type KernelEntryType = unsafe extern "Rust" fn(hart_id: usize, arg: *const BootArg) -> !;
