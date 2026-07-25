#![no_std]

pub use memory_addr::{PhysAddr, PhysAddrRange};

pub use ecraos_boot_macros::*;

/// The expected size of the boot stack (16 KiB).
pub const BOOTSTACK_SIZE: usize = 16 * 1024;

/// The argument provided by the bootloader or firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformBootArg {
    /// The Multiboot information structure.
    Multiboot(PhysAddr),
    /// The Device Tree.
    DeviceTree(PhysAddr),
    /// No argument provided.
    None,
}

/// The argument provided by the `ecraos_boot` during boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootArg {
    /// The physical address range occupied by the loader.
    ///
    /// This covers the loader's `.text`, `.rodata`, `.data` (including boot
    /// stack), but excludes the kernel payload section.
    pub loader_range: PhysAddrRange,
    pub plat_arg: PlatformBootArg,
}

/// The expected signature of the kernel entry function, i.e. the function that
/// is marked with [`kernel_entry`].
pub type KernelEntryType = unsafe extern "Rust" fn(hart_id: usize, arg: *const BootArg) -> !;
