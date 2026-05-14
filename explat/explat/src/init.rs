//! Initialization hooks: early and later platform bring-up.

use heapless::Vec as HeaplessVec;

pub use exboot::PlatformArg;

use crate::crate_interface::def_interface;

/// The maximum number of memory regions that can be collected during early
/// platform initialization.
pub const MAX_MEM_REGIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlyMemoryRegion {
    pub start: usize,
    pub size: usize,
}

pub type EarlyMemoryRegions = HeaplessVec<EarlyMemoryRegion, MAX_MEM_REGIONS>;

/// The support and enablement status of a half of the virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAHalfStatus {
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

/// The memory information collected during early platform initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyMemoryInfo {
    /// The support for the lower part of the virtual address space.
    pub va_lower_half_status: VAHalfStatus,
    /// The support for the upper part of the virtual address space.
    pub va_upper_half_status: VAHalfStatus,
    /// The memory regions.
    pub memory_regions: EarlyMemoryRegions,
}

/// The result of early platform initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyInitResult {
    /// The memory information collected during early platform initialization.
    pub memory_info: EarlyMemoryInfo,
}

/// Platform initialization contract invoked from portable kernel code.
#[def_interface(gen_caller)]
pub trait InitIf {
    /// Early platform initialization.
    ///
    /// This method should be called immediately after the kernel entry runs,
    /// and should perform the early platform initialization. This method will
    /// be called only once, on the bootstrap processor.
    ///
    /// When this method is called, the following conditions are met:
    ///
    /// - The CPU is in its desired working state (for example, in long mode in
    ///   the x86-64 architecture).
    /// - Virtual memory and paging are enabled with an identity mapping.
    /// - Allocation is not yet available.
    /// - Interrupts are disabled.
    ///
    /// This method should:
    /// - Initialize the debug console.
    /// - Collect memory information.
    ///
    fn init_early(arg: PlatformArg) -> EarlyInitResult;
    /// Later platform initialization. Yet to be implemented.
    fn init_later();
}
