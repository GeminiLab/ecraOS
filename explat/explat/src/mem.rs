use core::fmt;

use heapless::Vec as HeaplessVec;

use exboot::{PhysAddrRange, PlatformBootArg};

use crate::reexport::crate_interface::def_interface;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryRegionFlags: usize {
        /// Readable.
        const READ         = 1 << 0;
        /// Writable.
        const WRITE        = 1 << 1;
        /// Executable.
        const EXECUTE      = 1 << 2;
        /// Device memory (e.g., MMIO regions).
        const DEVICE       = 1 << 4;
        /// Uncachable memory (e.g., framebuffer).
        const UNCACHED     = 1 << 5;
        /// Reserved memory, do not use for allocation.
        const RESERVED     = 1 << 6;
        /// Free memory for allocation.
        const FREE         = 1 << 7;
        /// Memory region used for boot services.
        const BOOT_SERVICE = 1 << 8;
    }
}

/// The default flags for a normal RAM region (readable, writable and free).
pub const DEFAULT_RAM_FLAGS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::FREE);

/// The default flags for a reserved memory region (readable, writable, and
/// reserved).
pub const DEFAULT_RESERVED_FLAGS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::RESERVED);

/// A physical memory region with associated flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// The physical address range of the region.
    pub range: PhysAddrRange,
    /// The flags describing the properties of the region.
    pub flags: MemoryRegionFlags,
}

/// The maximum number of memory regions that can be collected by
/// [`boot_mem_regions`].
pub const MAX_BOOT_MEM_REGIONS: usize = 48;

/// The memory regions collected by [`boot_mem_regions`].
pub type MemoryRegions = HeaplessVec<MemoryRegion, MAX_BOOT_MEM_REGIONS>;

/// The maximum number of virtual address space modes that can be supported by
/// the platform.
///
/// This value is chosen to make [`VirtAddrSpace`] 256 bytes long. Also, AArch64
/// supports up to 6 modes for each half of the virtual address space (4/16/64
/// KiB page size, 48/52 bit VA, T<n>SZ not considered), so 48 seems to be a
/// always-reasonably-large-enough value.
pub const MAX_VA_MODES: usize = 48;

/// The properties of a virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddrSpaceProps {
    /// The shift (log2) of the page size.
    pub page_shift: u8,
    /// The number of valid bits in the virtual address.
    pub va_bits: u8,
}

/// A mode of the virtual address space supported by the platform.
///
/// Four modes are defined:
///
/// - [`VirtAddrSpaceMode::LowerOnly`]: Only the lower half of the virtual
///   address space is supported.
/// - [`VirtAddrSpaceMode::UpperOnly`]: Only the upper half of the virtual
///   address space is supported.
/// - [`VirtAddrSpaceMode::Independent`]: The lower and upper halves of the
///   virtual address space can be managed independently.
/// - [`VirtAddrSpaceMode::Unified`]: The lower and upper halves of the virtual
///   address space are managed by a single page table.
///
/// [`VirtAddrSpaceProps`] don't contains the total number of bits in the
/// virtual address space, which is assumed to be the number of bits in `usize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtAddrSpaceMode {
    /// Only the lower half of the virtual address space is supported.
    LowerOnly(VirtAddrSpaceProps),
    /// Only the upper half of the virtual address space is supported.
    UpperOnly(VirtAddrSpaceProps),
    /// The lower and upper halves of the virtual address space can be managed
    /// independently.
    ///
    /// In this case, `lower` and `upper` store the properties of the lower and
    /// upper halves of the virtual address space, respectively.
    Independent {
        lower: VirtAddrSpaceProps,
        upper: VirtAddrSpaceProps,
    },
    /// The lower and upper halves of the virtual address space are managed by
    /// a single page table.
    ///
    /// In this case, both halves of the virtual address space have only 1/2 of
    /// the total space defined by [`VirtAddrSpaceProps::va_bits`].
    Unified(VirtAddrSpaceProps),
}

impl fmt::Display for VirtAddrSpaceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        type P = VirtAddrSpaceProps;
        match *self {
            VirtAddrSpaceMode::LowerOnly(P {
                page_shift,
                va_bits,
            }) => {
                let min = 0usize;
                let max = 1usize.wrapping_shl(va_bits as _).wrapping_sub(1);
                let page_size = 1usize.wrapping_shl(page_shift as _);
                write!(
                    f,
                    "LowerOnly, {:#x}..{:#x}, page size {:#x}",
                    min, max, page_size
                )
            }
            VirtAddrSpaceMode::UpperOnly(P {
                page_shift,
                va_bits,
            }) => {
                let min = 1usize.wrapping_shl(va_bits as _).wrapping_neg();
                let max = usize::MAX;
                let page_size = 1usize.wrapping_shl(page_shift as _);
                write!(
                    f,
                    "UpperOnly, {:#x}..{:#x}, page size {:#x}",
                    min, max, page_size
                )
            }
            VirtAddrSpaceMode::Independent { lower, upper } => {
                let lower_min = 0usize;
                let lower_max = 1usize.wrapping_shl(lower.va_bits as _).wrapping_sub(1);
                let lower_page_size = 1usize.wrapping_shl(lower.page_shift as _);
                let upper_min = 1usize.wrapping_shl(upper.va_bits as _).wrapping_neg();
                let upper_max = usize::MAX;
                let upper_page_size = 1usize.wrapping_shl(upper.page_shift as _);
                write!(
                    f,
                    "Independent, lower: {:#x}..{:#x}, lower page size {:#x}, upper: {:#x}..{:#x}, upper page size {:#x}",
                    lower_min, lower_max, lower_page_size, upper_min, upper_max, upper_page_size
                )
            }
            VirtAddrSpaceMode::Unified(P {
                page_shift,
                va_bits,
            }) => {
                let lower_min = 0usize;
                let lower_max = 1usize.wrapping_shl((va_bits - 1) as _).wrapping_sub(1);
                let upper_min = 1usize.wrapping_shl((va_bits - 1) as _).wrapping_neg();
                let upper_max = usize::MAX;
                let page_size = 1usize.wrapping_shl(page_shift as _);
                write!(
                    f,
                    "Unified, lower: {:#x}..{:#x}, upper: {:#x}..{:#x}, page size {:#x}",
                    lower_min, lower_max, upper_min, upper_max, page_size
                )
            }
        }
    }
}

/// The supported and current virtual address space modes.
///
/// The current mode is the mode at the index `current_index` in the list
/// `modes`.
#[derive(Debug, Clone)]
pub struct VirtAddrSpaceModes {
    /// The index of the current virtual address space mode.
    pub current_index: usize,
    /// The supported virtual address space modes.
    pub modes: HeaplessVec<VirtAddrSpaceMode, MAX_VA_MODES>,
}

#[allow(clippy::new_without_default)]
impl VirtAddrSpaceModes {
    pub fn new() -> Self {
        Self {
            current_index: 0,
            modes: HeaplessVec::new(),
        }
    }
}

#[def_interface(gen_caller)]
pub trait MemIf {
    /// Collect the memory regions from the platform boot argument.
    fn boot_mem_regions(arg: PlatformBootArg) -> Option<MemoryRegions>;

    /// Get the supported and current virtual address space modes.
    fn virt_addr_space_modes() -> VirtAddrSpaceModes;

    /// Set the current virtual address space mode.
    ///
    /// # Panics
    ///
    /// This function will and should panic if the specified mode is not in the
    /// list returned by [`virt_addr_space_modes`], and not supported by the
    /// platform.
    fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode);
}
