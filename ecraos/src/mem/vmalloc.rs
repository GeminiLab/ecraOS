use alloc::{boxed::Box, vec::Vec};

use exboot::PhysAddrRange;
use expt::pte::MappingFlags;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::info;
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
    #[error("the range is outside of the allocator's range")]
    OutOfRange,
    #[error("no free range found")]
    NoFreeRange,
    #[error("the range is not aligned to the page size")]
    RangeNotAligned,
    #[error("the range is empty (zero-sized)")]
    EmptyRange,
    #[error("the range is outside of the outer range")]
    OutOfOuterRange,
    #[error("allocation error")]
    AllocationError,
    #[error("page table error")]
    PageTableError,
    #[error("the size of range does not match the number of pages")]
    RangeSizeMismatch,
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
        Self::validate_range(range, 1 << page_size_shift)?;

        Ok(Self {
            ranges: RangeSet::new(range),
            page_size_shift,
        })
    }

    #[expect(unused)]
    pub fn alloc_manual(&mut self, page_count: usize) -> Result<VirtAddrRange, VMAllocError> {
        let size = page_count << self.page_size_shift;
        let range = self
            .ranges
            .find_free_range(size)
            .ok_or(VMAllocError::NoFreeRange)?;

        debug_assert!(range.size() >= size);

        let range = VirtAddrRange::from_start_size(range.start, size);
        self.ranges
            .try_insert(range, VMAllocRange::Manual)
            .unwrap_or_else(|_| {
                unreachable!("the range should not overlap with an existing range");
            });
        Ok(range)
    }

    pub fn alloc_allocated(
        &mut self,
        page_count: usize,
        guard_pages: usize,
        flags: MappingFlags,
    ) -> Result<VirtAddrRange, VMAllocError> {
        let total_pages = page_count + guard_pages * 2;
        let total_size = total_pages << self.page_size_shift;
        let full_range = self
            .ranges
            .find_free_range(total_size)
            .ok_or(VMAllocError::NoFreeRange)?;

        debug_assert!(full_range.size() >= total_size);
        let full_range = VirtAddrRange::from_start_size(full_range.start, total_size);

        let allocated_range = VirtAddrRange::new(
            full_range.start + (guard_pages << self.page_size_shift),
            full_range.end - (guard_pages << self.page_size_shift),
        );

        let mut phys_pages = Vec::with_capacity(page_count);
        for index in 0..page_count {
            let virt_addr = allocated_range.start + (index << self.page_size_shift);
            let phys_addr =
                mem::palloc::alloc_frame().map_err(|_| VMAllocError::AllocationError)?;

            phys_pages.push(phys_addr);
            mem::vmm::with_page_table(|pt| {
                pt.map::<mem::vmm::TmpGoodPagingHandler>(
                    virt_addr,
                    phys_addr,
                    1 << self.page_size_shift,
                    flags,
                )
            })
            .map_err(|_| VMAllocError::PageTableError)?;
        }

        self.ranges
            .try_insert(
                full_range,
                VMAllocRange::Allocated {
                    allocated_range,
                    pages: phys_pages.into_boxed_slice(),
                },
            )
            .unwrap_or_else(|_| {
                unreachable!("the range should not overlap with an existing range");
            });

        Ok(allocated_range)
    }

    #[expect(unused)]
    pub fn add_manual(&mut self, range: VirtAddrRange) -> Result<(), VMAllocError> {
        Self::validate_range(range, self.page_size())?;
        self.ranges.try_insert(range, VMAllocRange::Manual)?;
        Ok(())
    }

    pub fn add_allocated_pages(
        &mut self,
        range: VirtAddrRange,
        allocated_range: VirtAddrRange,
        pages: Box<[PhysAddr]>,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        Self::validate_range(range, page_size)?;
        Self::validate_range(allocated_range, page_size)?;

        if !range.contains_range(allocated_range) {
            return Err(VMAllocError::OutOfOuterRange);
        }

        if pages.len() != allocated_range.size() / page_size {
            return Err(VMAllocError::RangeSizeMismatch);
        }

        self.ranges.try_insert(
            range,
            VMAllocRange::Allocated {
                allocated_range,
                pages,
            },
        )?;
        Ok(())
    }

    pub fn add_allocated_range(
        &mut self,
        range: VirtAddrRange,
        allocated_range: VirtAddrRange,
        physical_range: PhysAddrRange,
    ) -> Result<(), VMAllocError> {
        // Other parameters are validated by add_allocated_pages
        if allocated_range.size() != physical_range.size() {
            return Err(VMAllocError::RangeSizeMismatch);
        }

        let page_count = physical_range.size() >> self.page_size_shift;
        let mut pages = Vec::with_capacity(page_count);
        for index in 0..page_count {
            let phys_addr = physical_range.start + (index << self.page_size_shift);
            pages.push(phys_addr);
        }

        self.add_allocated_pages(range, allocated_range, pages.into_boxed_slice())
    }

    pub fn virt_to_phys(&self, addr: VirtAddr) -> Option<PhysAddr> {
        let (_, range) = self.ranges.find(addr)?;
        let VMAllocRange::Allocated {
            allocated_range,
            pages,
        } = range
        else {
            return None;
        };

        if !allocated_range.contains(addr) {
            return None;
        }

        let offset = addr.as_usize() - allocated_range.start.as_usize();
        let page_index = offset >> self.page_size_shift;
        let page_offset = offset & (self.page_size() - 1);
        pages.get(page_index).map(|page| *page + page_offset)
    }

    #[inline]
    fn page_size(&self) -> usize {
        1usize << self.page_size_shift
    }

    fn is_range_aligned(range: VirtAddrRange, page_size: usize) -> bool {
        range.start.is_aligned(page_size) && range.end.is_aligned(page_size)
    }

    fn validate_range(range: VirtAddrRange, page_size: usize) -> Result<(), VMAllocError> {
        if !Self::is_range_aligned(range, page_size) {
            return Err(VMAllocError::RangeNotAligned);
        }

        if range.is_empty() {
            return Err(VMAllocError::EmptyRange);
        }

        Ok(())
    }
}

pub static VMALLOC: LazyInit<SpinNoIrq<VMAllocator>> = LazyInit::new();

pub(super) fn init_vmalloc(vmalloc_range: VirtAddrRange, page_size_shift: usize) {
    info!("Initializing VMAllocator...");
    VMALLOC.init_once(SpinNoIrq::new(
        VMAllocator::new(vmalloc_range, page_size_shift).unwrap(),
    ));
}

pub fn virt_to_phys(addr: VirtAddr) -> Option<PhysAddr> {
    VMALLOC.get()?.lock().virt_to_phys(addr)
}
