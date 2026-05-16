use heapless::Vec as HeaplessVec;

use exboot::{PhysAddrRange, PlatformBootArg};

use crate::reexport::crate_interface::def_interface;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMemoryRegionType {
    /// The memory region is a reserved region.
    Reserved,
    /// The memory region is a usable region.
    Usable,
    /// The memory region is used for boot services.
    BootService,
}

bitflags::bitflags! {
    /// The flags of a physical memory region.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootMemoryRegionFlags: usize {
        /// Readable.
        const READ          = 1 << 0;
        /// Writable.
        const WRITE         = 1 << 1;
        /// Executable.
        const EXECUTE       = 1 << 2;
        /// Device memory. (e.g., MMIO regions)
        const DEVICE        = 1 << 4;
        /// Uncachable memory. (e.g., framebuffer)
        const UNCACHED      = 1 << 5;
        /// Reserved memory, do not use for allocation.
        const RESERVED      = 1 << 6;
        /// Free memory for allocation.
        const FREE          = 1 << 7;
        /// Memory region used for boot services.
        const BOOT_SERVICE  = 1 << 8;
    }
}

/// The default flags for a normal memory region (readable, writable and allocatable).
pub const DEFAULT_RAM_FLAGS: BootMemoryRegionFlags = BootMemoryRegionFlags::READ
    .union(BootMemoryRegionFlags::WRITE)
    .union(BootMemoryRegionFlags::FREE);

/// The default flags for a reserved memory region (readable, writable, and reserved).
pub const DEFAULT_RESERVED_FLAGS: BootMemoryRegionFlags = BootMemoryRegionFlags::READ
    .union(BootMemoryRegionFlags::WRITE)
    .union(BootMemoryRegionFlags::RESERVED);

/// The default flags for a MMIO region (readable, writable, device, and reserved).
pub const DEFAULT_MMIO_FLAGS: BootMemoryRegionFlags = BootMemoryRegionFlags::READ
    .union(BootMemoryRegionFlags::WRITE)
    .union(BootMemoryRegionFlags::DEVICE)
    .union(BootMemoryRegionFlags::RESERVED);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootMemoryRegion {
    pub range: PhysAddrRange,
    pub type_: BootMemoryRegionType,
}

/// The maximum number of memory regions that can be collected by
/// [`boot_mem_info`].
pub const MAX_BOOT_MEM_REGIONS: usize = 48;

/// The memory regions collected by [`boot_mem_info`].
pub type BootMemoryRegions = HeaplessVec<BootMemoryRegion, MAX_BOOT_MEM_REGIONS>;

/// The support and enablement status of a half of the virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtAddrSpaceHalfStatus {
    /// This half of the virtual address space is not supported.
    NotSupported,
    /// This half of the virtual address space is supported, but currently
    /// disabled. At most `max_va_bits` bits are supported **in this half** of
    /// the virtual address space.
    Disabled { max_bits: u32 },
    /// This half of the virtual address space is enabled. `current_va_bits`
    /// bits are currently enabled, while at most `max_va_bits` bits are
    /// supported, **in this half** of the virtual address space.
    Enabled { current_bits: u32, max_bits: u32 },
}

/// The support and status of the virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddrSpaceStatus {
    /// The support and status of the lower half of the virtual address space.
    pub lower_half: VirtAddrSpaceHalfStatus,
    /// The support and status of the upper half of the virtual address space.
    pub upper_half: VirtAddrSpaceHalfStatus,
}

#[def_interface(gen_caller)]
pub trait MemIf {
    /// Collect the memory regions from the platform boot argument.
    fn boot_mem_regions(arg: PlatformBootArg) -> Option<BootMemoryRegions>;

    /// Get the support and status of the virtual address space.
    fn virt_addr_space_status() -> VirtAddrSpaceStatus;
}
