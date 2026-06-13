use core::fmt;

use exboot::{PhysAddrRange, PlatformBootArg};
use expt::opaque::OpaquePageTableType;
use heapless::Vec as HeaplessVec;
use memory_addr::VirtAddr;

use crate::reexport::crate_interface::def_interface;

bitflags::bitflags! {
    /// The flags for a memory region.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryRegionFlags: usize {
        // Property flags
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

        // Role flags
        /// Reserved memory, do not use for allocation.
        const RESERVED     = 1 << 6;
        /// Free memory for allocation.
        const FREE         = 1 << 7;
        /// Memory region used for boot services. May be reclaimed later by the
        /// OS, if the OS is sure it won't be used again.
        const BOOT_SERVICE = 1 << 8;
        /// Memory region used for the kernel.
        const KERNEL       = 1 << 9;
    }
}

impl MemoryRegionFlags {
    /// Checks if the role flags are consistent, i.e. exactly one of the role
    /// flags is set.
    pub fn role_sanity_check(&self) -> bool {
        const ROLE_BITS: usize = MemoryRegionFlags::RESERVED.bits()
            | MemoryRegionFlags::FREE.bits()
            | MemoryRegionFlags::BOOT_SERVICE.bits()
            | MemoryRegionFlags::KERNEL.bits();
        let role_flags = self.bits() & ROLE_BITS;
        role_flags.count_ones() == 1
    }
}

impl fmt::Display for MemoryRegionFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        macro_rules! fmt_flag_bits {
            (- $(, $($tt:tt)*)?) => {
                write!(f, "-")?;
                $( fmt_flag_bits!($($tt)*); )?
            };
            ($test_bit:ident:$write_char:literal $(, $($tt:tt)*)?) => {
                write!(f, "{}", if self.contains(Self::$test_bit) { $write_char } else { '-' })?;
                $( fmt_flag_bits!($($tt)*); )?
            };
            () => {};
        }

        fmt_flag_bits!(
            READ:'R', WRITE:'W', EXECUTE:'X', -,
            DEVICE:'D', UNCACHED:'U',
            RESERVED:'R', FREE:'F', BOOT_SERVICE:'B', KERNEL:'K'
        );

        Ok(())
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

/// The default description for a normal free RAM region.
pub const DEFAULT_RAM_DESC: &str = "free memory";
/// The default description for a reserved memory region.
pub const DEFAULT_RESERVED_DESC: &str = "reserved";

/// A physical memory region with associated flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// The physical address range of the region.
    pub range: PhysAddrRange,
    /// The flags describing the properties of the region.
    pub flags: MemoryRegionFlags,
    /// The human-readable description for the region.
    pub desc: &'static str,
}

/// The maximum number of memory regions that can be collected by
/// [`raw_mem_regions`].
pub const MAX_RAW_MEM_REGIONS: usize = 32;

/// The memory regions collected by [`raw_mem_regions`].
pub type RawMemoryRegions = HeaplessVec<MemoryRegion, MAX_RAW_MEM_REGIONS>;

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

impl VirtAddrSpaceMode {
    const fn lower_range_inc(low_va_bits: u8) -> (usize, usize) {
        (0, 1usize.wrapping_shl(low_va_bits as _).wrapping_sub(1))
    }

    const fn upper_range_inc(high_va_bits: u8) -> (usize, usize) {
        (
            1usize.wrapping_shl(high_va_bits as _).wrapping_neg(),
            usize::MAX,
        )
    }

    const fn page_size(page_shift: u8) -> usize {
        1usize.wrapping_shl(page_shift as _)
    }
}

impl fmt::Display for VirtAddrSpaceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        type P = VirtAddrSpaceProps;
        match *self {
            VirtAddrSpaceMode::LowerOnly(P {
                page_shift,
                va_bits,
            }) => {
                let (min, max) = Self::lower_range_inc(va_bits);
                let page_size = Self::page_size(page_shift);
                write!(
                    f,
                    "LowerOnly, {:#x}..={:#x}, page size {:#x}",
                    min, max, page_size
                )
            }
            VirtAddrSpaceMode::UpperOnly(P {
                page_shift,
                va_bits,
            }) => {
                let (min, max) = Self::upper_range_inc(va_bits);
                let page_size = Self::page_size(page_shift);
                write!(
                    f,
                    "UpperOnly, {:#x}..={:#x}, page size {:#x}",
                    min, max, page_size
                )
            }
            VirtAddrSpaceMode::Independent { lower, upper } => {
                let (lmin, lmax) = Self::lower_range_inc(lower.va_bits);
                let (umin, umax) = Self::upper_range_inc(upper.va_bits);
                let lpage_size = Self::page_size(lower.page_shift);
                let upage_size = Self::page_size(upper.page_shift);
                write!(
                    f,
                    "Independent, lower: {:#x}..={:#x}, lower page size {:#x}, upper: {:#x}..={:#x}, upper page size {:#x}",
                    lmin, lmax, lpage_size, umin, umax, upage_size
                )
            }
            VirtAddrSpaceMode::Unified(P {
                page_shift,
                va_bits,
            }) => {
                let (lmin, lmax) = Self::lower_range_inc(va_bits - 1);
                let (umin, umax) = Self::upper_range_inc(va_bits - 1);
                let page_size = Self::page_size(page_shift);
                write!(
                    f,
                    "Unified, lower: {:#x}..={:#x}, upper: {:#x}..={:#x}, page size {:#x}",
                    lmin, lmax, umin, umax, page_size
                )
            }
        }
    }
}

/// The maximum number of virtual address space modes that can be supported by
/// the platform.
///
/// This value is chosen to make [`VirtAddrSpace`] 256 bytes long. Also, AArch64
/// supports up to 6 modes for each half of the virtual address space (4/16/64
/// KiB page size, 48/52 bit VA, T<n>SZ not considered), so 48 seems to be a
/// always-reasonably-large-enough value.
pub const MAX_VA_MODES: usize = 48;

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
    /// Collects the memory regions from the platform boot argument.
    ///
    /// The returned memory regions should not cantain the
    /// [`MemoryRegionFlags::KERNEL`] flag, which is supposed to be used by the
    /// kernel itself.
    fn raw_mem_regions(arg: PlatformBootArg) -> RawMemoryRegions;

    /// Gets the supported and current virtual address space modes.
    fn virt_addr_space_modes() -> VirtAddrSpaceModes;

    /// Sets the current virtual address space mode.
    ///
    /// It's guaranteed that this function will only be called when an identical mapping is
    /// currently active. It's also required that when this function returns, a valid identical
    /// mapping is active.
    ///
    /// # Panics
    ///
    /// This function will and should panic if the specified mode is not in the
    /// list returned by [`virt_addr_space_modes`], and not supported by the
    /// platform.
    fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode);

    /// Gets the [`OpaquePageTableType`] for the specified virtual address space mode.
    ///
    /// # Panics
    ///
    /// This function will and should panic if the specified mode is not in the
    /// list returned by [`virt_addr_space_modes`], and not supported by the
    /// platform.
    fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr>;
}
