use alloc::{boxed::Box, vec::Vec};

use explat::mem::MemoryRegionFlags;
use memory_addr::{MemoryAddr, PhysAddr, VirtAddr, VirtAddrRange};
use memory_range_set::{RangeSet, TryInsertError};

use crate::mem;

/// A range of virtual memory that is managed by the [`VMAllocator`].
pub enum VMAllocRange {
    /// A range whose mapping is manually managed by the user.
    Manual,
    /// A range whose mapping is allocated by the [`VMAllocator`].
    Allocated {
        allocated_range: VirtAddrRange,
        pages: Box<[PhysAddr]>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum VMAllocError {
    #[error("the range overlaps with an existing range")]
    Overlap,
    #[error("the range is outside the allocator's range")]
    OutOfRange,
    #[error("no free range found")]
    NoFreeRange,
    #[error("the range is not aligned to the page size")]
    RangeNotAligned,
    #[error("buddy allocator error: {0}")]
    BuddyError(
        #[source]
        #[from]
        exbuddy::BuddyError,
    ),
}

impl<A: MemoryAddr, P> From<TryInsertError<A, P>> for VMAllocError {
    fn from(error: TryInsertError<A, P>) -> Self {
        match error {
            TryInsertError::Overlap { .. } => VMAllocError::Overlap,
            TryInsertError::OutOfLimit { .. } => VMAllocError::OutOfRange,
        }
    }
}

/// A virtual memory allocator.
///
/// This allocator is used to allocate (maybe non-contiguous physically) memory and map it to a
/// contiguous range in the `vmalloc` area of the virtual address space.
///
/// This allocator depends on the physical memory allocator directly.
pub struct VMAllocator {
    ranges: RangeSet<VirtAddr, VMAllocRange>,
    page_size_shift: usize,
}

impl VMAllocator {
    pub fn new(range: VirtAddrRange, page_size_shift: usize) -> Result<Self, VMAllocError> {
        let page_size = 1usize << page_size_shift;
        if !range.start.is_aligned(page_size) || !range.end.is_aligned(page_size) {
            return Err(VMAllocError::RangeNotAligned);
        }

        Ok(Self {
            ranges: RangeSet::new(range),
            page_size_shift,
        })
    }

    pub fn alloc_manual(&mut self, page_count: usize) -> Result<VirtAddrRange, VMAllocError> {
        let size = page_count << self.page_size_shift;
        let range = self
            .ranges
            .find_free_range(size)
            .ok_or(VMAllocError::NoFreeRange)?;
        self.ranges.try_insert(range, VMAllocRange::Manual)?;
        Ok(range)
    }

    pub fn alloc_allocated(
        &mut self,
        page_count: usize,
        guard_pages: usize,
        flags: MemoryRegionFlags,
    ) -> Result<VirtAddrRange, VMAllocError> {
        let total_pages = page_count + guard_pages * 2;
        let total_size = total_pages << self.page_size_shift;
        let full_range = self
            .ranges
            .find_free_range(total_size)
            .ok_or(VMAllocError::NoFreeRange)?;
        let allocated_range = VirtAddrRange::new(
            full_range.start + (guard_pages << self.page_size_shift),
            full_range.end - (guard_pages << self.page_size_shift),
        );

        let mut phys_pages = Vec::with_capacity(page_count);
        for index in 0..page_count {
            let virt_addr = allocated_range.start + (index << self.page_size_shift);
            let phys_addr = mem::alloc::alloc_frame()?;

            phys_pages.push(phys_addr);
        }
        todo!();
    }
}
