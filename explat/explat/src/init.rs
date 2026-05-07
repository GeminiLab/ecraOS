//! Initialization hooks: early and later platform bring-up.

use heapless::Vec as HeaplessVec;

use crate::crate_interface::def_interface;

pub const MAX_MEM_REGIONS: usize = 64;

pub struct EarlyMemoryRegion {
    pub start: usize,
    pub size: usize,
}

pub type EarlyMemoryRegions = HeaplessVec<EarlyMemoryRegion, MAX_MEM_REGIONS>;

pub struct EarlyInitResult {
    pub memory_regions: EarlyMemoryRegions,
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
    fn init_early(arg: usize) -> EarlyInitResult;
    /// Later platform initialization.
    fn init_later();
}
