//! Vmalloc range allocator.
//!
//! Tracks occupied ranges in the kernel vmalloc area, records per-page
//! virtual-to-physical mappings, and owns page-table mappings for ranges that
//! are mapped through this allocator.

use alloc::{boxed::Box, vec::Vec};
use core::range::Range;

use ecraos_boot::PhysAddrRange;
use expt::pte::MappingFlags;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::{error, info, warn};
use memory_addr::{MemoryAddr, PhysAddr, VirtAddr, VirtAddrRange, pa};
use memory_range_set::{RangeSet, TryInsertError};

use crate::mem::{self, allocs::palloc};

/// Per-page vmalloc mapping state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum VMAllocPageStatus {
    /// A page without a vmalloc mapping.
    NotMapped,
    /// A page backed by a physical frame owned by the vmalloc allocator.
    Owned {
        /// Flags used for the page-table mapping.
        flags: MappingFlags,
    },
    /// A page backed by a physical frame owned by external code.
    External {
        /// Flags used for the page-table mapping.
        flags: MappingFlags,
    },
}

impl VMAllocPageStatus {
    /// Returns whether the page has a recorded mapping.
    const fn is_mapped(self) -> bool {
        !matches!(self, Self::NotMapped)
    }

    /// Returns the flags used for the page-table mapping. Returns `MappingFlags::empty()` for
    /// [`Self::NotMapped`].
    const fn flags(self) -> MappingFlags {
        match self {
            Self::Owned { flags } => flags,
            Self::External { flags } => flags,
            Self::NotMapped => MappingFlags::empty(),
        }
    }

    /// Returns whether the page is owned by the vmalloc allocator.
    const fn is_owned(self) -> bool {
        matches!(self, Self::Owned { .. })
    }
}

/// Per-page mapping records for the non-guard part of a vmalloc range.
///
/// Indexed by the page index within the vmalloc range, instead of the virtual address, i.e. this
/// struct is va-unaware.
struct VMAllocPageList {
    /// Physical pages recorded for mapped slots.
    phys_pages: Box<[PhysAddr]>,
    /// Mapping state for each local page index.
    statuses: Box<[VMAllocPageStatus]>,
}

impl VMAllocPageList {
    /// Creates a page list whose pages are initially unmapped.
    fn new(page_count: usize) -> Self {
        let mut phys_pages = Vec::with_capacity(page_count);
        let mut statuses = Vec::with_capacity(page_count);

        for _ in 0..page_count {
            phys_pages.push(pa!(0));
            statuses.push(VMAllocPageStatus::NotMapped);
        }

        Self {
            phys_pages: phys_pages.into_boxed_slice(),
            statuses: statuses.into_boxed_slice(),
        }
    }

    /// Creates a page list from existing external page mappings.
    fn new_external(phys_pages: Box<[PhysAddr]>, flags: MappingFlags) -> Self {
        let mut statuses = Vec::with_capacity(phys_pages.len());
        for _ in 0..phys_pages.len() {
            statuses.push(VMAllocPageStatus::External { flags });
        }

        Self {
            phys_pages,
            statuses: statuses.into_boxed_slice(),
        }
    }

    /// Returns the status of a local page slot.
    fn status(&self, index: usize) -> VMAllocPageStatus {
        self.statuses[index]
    }

    /// Returns whether a local page slot is mapped.
    fn is_mapped(&self, index: usize) -> bool {
        self.status(index).is_mapped()
    }

    /// Returns the physical address recorded for a mapped local page slot.
    fn phys_addr(&self, index: usize) -> PhysAddr {
        self.phys_pages[index]
    }

    /// Marks a local page slot as mapped.
    ///
    /// Calling this function with `status` set to `NotMapped` has the same effect as calling `mark_unmapped`.
    fn mark_mapped(&mut self, index: usize, phys_addr: PhysAddr, status: VMAllocPageStatus) {
        self.phys_pages[index] = phys_addr;
        self.statuses[index] = status;
    }

    /// Marks a local page slot as unmapped.
    fn mark_unmapped(&mut self, index: usize) {
        self.phys_pages[index] = pa!(0);
        self.statuses[index] = VMAllocPageStatus::NotMapped;
    }
}

/// An allocated vmalloc range.
pub(crate) struct VMAllocRange {
    /// The non-guard part of the reservation.
    non_guard_range: VirtAddrRange,
    /// Per-page mappings for the non-guard part.
    pages: VMAllocPageList,
}

impl VMAllocRange {
    /// Creates a new unmapped vmalloc range.
    fn new(non_guard_range: VirtAddrRange, page_count: usize) -> Self {
        Self {
            non_guard_range,
            pages: VMAllocPageList::new(page_count),
        }
    }

    /// Creates a vmalloc range from existing external mappings.
    fn new_external(
        non_guard_range: VirtAddrRange,
        phys_pages: Box<[PhysAddr]>,
        flags: MappingFlags,
    ) -> Self {
        Self {
            non_guard_range,
            pages: VMAllocPageList::new_external(phys_pages, flags),
        }
    }

    /// Returns the non-guard range of the vmalloc range.
    fn non_guard_range(&self) -> VirtAddrRange {
        self.non_guard_range
    }

    /// Allocates physical pages and maps them into a subrange inside this vmalloc range.
    fn map_alloc(
        &mut self,
        range: VirtAddrRange,
        page_size: usize,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        self.map_inner(
            range,
            page_size,
            VMAllocPageStatus::Owned { flags },
            |_, _| match palloc::alloc_frame() {
                Ok(page) => Ok(page),
                Err(_) => Err(VMAllocError::AllocationError),
            },
            |_, _, previous_pa_list| {
                for page in previous_pa_list {
                    palloc::dealloc_frame(*page).unwrap_or_else(|e| {
                        panic!("failed to roll back vmalloc physical page {page:#x}: {e:?}");
                    });
                }
                Ok(())
            },
        )
    }

    /// Maps external contiguous physical pages into a subrange.
    fn map_phys_range(
        &mut self,
        range: VirtAddrRange,
        physical_range: PhysAddrRange,
        page_size: usize,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        if physical_range.size() != range.size() {
            return Err(VMAllocError::RangeSizeMismatch);
        }
        if !physical_range.start.is_aligned(page_size) || !physical_range.end.is_aligned(page_size)
        {
            return Err(VMAllocError::RangeNotAligned);
        }

        self.map_inner(
            range,
            page_size,
            VMAllocPageStatus::External { flags },
            |start_index: usize, current_index: usize| {
                Ok(physical_range.start + (current_index - start_index) * page_size)
            },
            |_start_index, _failing_index, _previous_pa_list| Ok(()),
        )
    }

    /// Maps external physical pages into a subrange.
    fn map_phys_pages(
        &mut self,
        range: VirtAddrRange,
        phys_pages: &[PhysAddr],
        page_size: usize,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_index_range = self.page_index_range(range, page_size)?;
        if phys_pages.len() != page_index_range.end - page_index_range.start {
            return Err(VMAllocError::RangeSizeMismatch);
        }

        Self::validate_phys_pages(phys_pages, page_size)?;

        self.map_inner(
            range,
            page_size,
            VMAllocPageStatus::External { flags },
            |start_index: usize, current_index: usize| {
                if current_index - start_index >= phys_pages.len() {
                    return Err(VMAllocError::RangeSizeMismatch);
                }
                Ok(phys_pages[current_index - start_index])
            },
            |_start_index, _failing_index, _previous_pa_list| Ok(()),
        )
    }

    /// Unmaps a subrange, skipping pages that are already unmapped.
    fn unmap(&mut self, range: VirtAddrRange, page_size: usize) -> Result<(), VMAllocError> {
        let range = range;
        let page_index_range = self.page_index_range(range, page_size)?;

        for index in page_index_range {
            let virt_addr = self.non_guard_range.start + index * page_size;

            if self.pages.is_mapped(index) {
                mem::vmm::with_page_table(|pt| {
                    pt.unmap::<mem::vmm::TmpGoodPagingHandler>(virt_addr, page_size)
                }).map_err(|e| {
                    error!("failed to unmap vmalloc page at {virt_addr:#x} while updating range {range:#x}: {e:?}");
                    VMAllocError::PageTableError
                })?;

                let phys_addr = self.pages.phys_addr(index);
                let status = self.pages.status(index);
                self.pages.mark_unmapped(index);

                if status.is_owned() {
                    palloc::dealloc_frame(phys_addr).map_err(|e| {
                        error!("failed to deallocate physical page {phys_addr:#x} at {virt_addr:#x} while unmapping vmalloc range {range:#x}: {e:?}");

                        error!("this will result in a memory leak");

                        let unprocessed_va_range = VirtAddrRange::new(virt_addr + page_size, range.end);
                        if !unprocessed_va_range.is_empty() {
                            error!(
                                "unprocessed virtual address range: {unprocessed_va_range:#x}"
                            );
                        }
                        VMAllocError::DeallocationError
                    })?;
                }
            }
        }

        Ok(())
    }

    /// Translates a virtual address inside this range to a physical address.
    fn virt_to_phys(&self, addr: VirtAddr, page_size: usize) -> Option<PhysAddr> {
        if !self.non_guard_range.contains(addr) {
            return None;
        }

        let offset = addr.as_usize() - self.non_guard_range.start.as_usize();
        let page_index = offset / page_size;
        if !self.pages.is_mapped(page_index) {
            return None;
        }

        Some(self.pages.phys_addr(page_index) + (offset % page_size))
    }

    /// Returns the local page index range for a virtual address range inside this vmalloc range.
    fn page_index_range(
        &self,
        range: VirtAddrRange,
        page_size: usize,
    ) -> Result<Range<usize>, VMAllocError> {
        VMAllocator::validate_range(range, page_size)?;
        if !self.non_guard_range.contains_range(range) {
            return Err(VMAllocError::OutOfOuterRange);
        }

        let start_offset = range.start.as_usize() - self.non_guard_range.start.as_usize();
        let page_count = range.size() / page_size;
        Ok(Range {
            start: start_offset / page_size,
            end: start_offset / page_size + page_count,
        })
    }

    fn ensure_index_range_not_mapped(&self, range: Range<usize>) -> Result<(), VMAllocError> {
        for index in range {
            if self.pages.is_mapped(index) {
                return Err(VMAllocError::AlreadyMapped);
            }
        }
        Ok(())
    }

    fn map_inner(
        &mut self,
        range: VirtAddrRange,
        page_size: usize,
        status: VMAllocPageStatus,
        mut pa_getter: impl FnMut(
            /* start_index */ usize,
            /* current_index */ usize,
        ) -> Result<PhysAddr, VMAllocError>,
        rollback: impl FnOnce(
            /* start_index */ usize,
            /* failing_index */ usize,
            /* previous_pa_list */ &[PhysAddr],
        ) -> Result<(), VMAllocError>,
    ) -> Result<(), VMAllocError> {
        debug_assert!(status.is_mapped());

        let page_index_range = self.page_index_range(range, page_size)?;
        self.ensure_index_range_not_mapped(page_index_range)?;

        let flags = status.flags();
        for index in page_index_range {
            let va = self.non_guard_range.start + index * page_size;

            // The result of the get-phys-addr-then-map-it operation.
            // - Ok(pa) if the operation succeeded.
            // - Err((e, None)) if the physical address could not be obtained.
            // - Err((e, Some(pa))) if the page-table mapping failed.
            let result = match pa_getter(page_index_range.start, index) {
                Ok(pa) => match mem::vmm::with_page_table(|pt| {
                    pt.map::<mem::vmm::TmpGoodPagingHandler>(va, pa, page_size, flags)
                }) {
                    Ok(_) => Ok(pa),
                    Err(e) => {
                        error!("failed to map vmalloc page at {va:#x} ({pa:#x}): {e:?}");
                        Err((VMAllocError::PageTableError, Some(pa)))
                    }
                },
                Err(e) => {
                    error!("failed to get phys addr for vmalloc page at {va:#x}: {e:?}");
                    Err((e, None))
                }
            };

            match result {
                Ok(pa) => self.pages.mark_mapped(index, pa, status),
                Err((e, current_pa)) => {
                    error!("while updating range {range:#x} (index: {index}) with {status:?}");
                    warn!("rolling back vmalloc map operation...");

                    // Try rolling back the changes we made so far.
                    let previous_range = Range {
                        start: page_index_range.start,
                        end: index,
                    };

                    // Roll back the page-table mappings.
                    for previous_index in previous_range {
                        let virt_addr = self.non_guard_range.start + previous_index * page_size;

                        mem::vmm::with_page_table(|pt| {
                            pt.unmap::<mem::vmm::TmpGoodPagingHandler>(virt_addr, page_size)
                        })
                        .expect("failed to roll back vmalloc page-table mapping at {virt_addr:#x}");
                    }

                    // Determine the index range to roll back the physical page assignments.
                    let pa_rollback_range = match current_pa {
                        Some(pa) => {
                            // Update the physical page assignment for the current index.
                            self.pages.phys_pages[index] = pa;
                            // Update the index range to roll back the current index also.
                            Range {
                                start: previous_range.start,
                                end: index + 1,
                            }
                        }
                        None => previous_range,
                    };

                    // Roll back the physical page assignments.
                    rollback(
                        page_index_range.start,
                        index,
                        &self.pages.phys_pages[pa_rollback_range],
                    )
                    .unwrap_or_else(|e| {
                        panic!(
                            "failed to rollback vmalloc operation after the previous error: {e:?}"
                        );
                    });

                    // Roll back the internal state.
                    for pa_rollback_index in pa_rollback_range {
                        self.pages.mark_unmapped(pa_rollback_index);
                    }

                    warn!("vmalloc map operation rolled back successfully");

                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Validates that all physical pages are page aligned.
    fn validate_phys_pages(phys_pages: &[PhysAddr], page_size: usize) -> Result<(), VMAllocError> {
        if phys_pages.iter().any(|page| !page.is_aligned(page_size)) {
            return Err(VMAllocError::RangeNotAligned);
        }
        Ok(())
    }
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
    #[error("the range already contains a mapped page")]
    AlreadyMapped,
    #[error("allocation error")]
    AllocationError,
    #[error("page table error")]
    PageTableError,
    #[error("the size of range does not match the number of pages")]
    RangeSizeMismatch,
    #[error("the address does not identify an allocated vmalloc range")]
    NotAllocated,
    #[error("the global vmalloc allocator is not initialized")]
    NotInitialized,
    #[error("physical page deallocation error")]
    DeallocationError,
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
/// The allocator reserves address ranges in the vmalloc area. Guard pages are
/// part of the reserved range but are not tracked in the per-page mapping list.
pub struct VMAllocator {
    /// Occupied ranges in the vmalloc area.
    ranges: RangeSet<VirtAddr, VMAllocRange>,
    /// Page size shift used by this allocator.
    page_size_shift: usize,
}

impl VMAllocator {
    /// Creates a vmalloc allocator for the given virtual range.
    pub fn new(range: VirtAddrRange, page_size_shift: usize) -> Result<Self, VMAllocError> {
        Self::validate_range(range, 1 << page_size_shift)?;

        Ok(Self {
            ranges: RangeSet::new(range),
            page_size_shift,
        })
    }

    /// Allocates a vmalloc range whose pages are initially unmapped.
    pub fn alloc_range(
        &mut self,
        page_count: usize,
        guard_pages: usize,
    ) -> Result<VirtAddrRange, VMAllocError> {
        if page_count == 0 {
            return Err(VMAllocError::EmptyRange);
        }

        let page_size = self.page_size();
        let total_pages = page_count + guard_pages * 2;
        let total_size = total_pages * page_size;
        let full_range = self
            .ranges
            .find_free_range(total_size)
            .ok_or(VMAllocError::NoFreeRange)?;

        debug_assert!(full_range.size() >= total_size);
        let full_range = VirtAddrRange::from_start_size(full_range.start, total_size);
        let mapped_range = VirtAddrRange::new(
            full_range.start + guard_pages * page_size,
            full_range.end - guard_pages * page_size,
        );

        self.ranges
            .try_insert(full_range, VMAllocRange::new(mapped_range, page_count))?;
        Ok(mapped_range)
    }

    /// Allocates a range and maps the whole non-guard span to owned pages.
    pub fn alloc_range_and_map_alloc(
        &mut self,
        page_count: usize,
        guard_pages: usize,
        flags: MappingFlags,
    ) -> Result<VirtAddrRange, VMAllocError> {
        let range = self.alloc_range(page_count, guard_pages)?;
        if let Err(error) = self.map_alloc(range, flags) {
            self.dealloc_range(range.start)
                .expect("failed to roll back vmalloc range");
            return Err(error);
        }
        Ok(range)
    }

    /// Allocates a range and maps the whole non-guard span to external contiguous pages.
    #[expect(unused)]
    pub fn alloc_range_and_map_phys_range(
        &mut self,
        guard_pages: usize,
        physical_range: PhysAddrRange,
        flags: MappingFlags,
    ) -> Result<VirtAddrRange, VMAllocError> {
        let page_size = self.page_size();
        Self::validate_phys_range(physical_range, page_size)?;
        let page_count = physical_range.size() / page_size;
        let range = self.alloc_range(page_count, guard_pages)?;
        if let Err(error) = self.map_phys_range(range, physical_range, flags) {
            self.dealloc_range(range.start)
                .expect("failed to roll back vmalloc range");
            return Err(error);
        }
        Ok(range)
    }

    /// Allocates a range and maps the whole non-guard span to external page list entries.
    #[expect(unused)]
    pub fn alloc_range_and_map_phys_pages(
        &mut self,
        guard_pages: usize,
        phys_pages: &[PhysAddr],
        flags: MappingFlags,
    ) -> Result<VirtAddrRange, VMAllocError> {
        let page_size = self.page_size();
        VMAllocRange::validate_phys_pages(phys_pages, page_size)?;
        let range = self.alloc_range(phys_pages.len(), guard_pages)?;
        if let Err(error) = self.map_phys_pages(range, phys_pages, flags) {
            self.dealloc_range(range.start)
                .expect("failed to roll back vmalloc range");
            return Err(error);
        }
        Ok(range)
    }

    /// Maps owned physical pages into an allocated vmalloc subrange.
    pub fn map_alloc(
        &mut self,
        range: VirtAddrRange,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        allocation.map_alloc(range, page_size, flags)
    }

    /// Maps an external contiguous physical range into an allocated vmalloc subrange.
    pub fn map_phys_range(
        &mut self,
        range: VirtAddrRange,
        physical_range: PhysAddrRange,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        allocation.map_phys_range(range, physical_range, page_size, flags)
    }

    /// Maps external physical pages into an allocated vmalloc subrange.
    pub fn map_phys_pages(
        &mut self,
        range: VirtAddrRange,
        phys_pages: &[PhysAddr],
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        allocation.map_phys_pages(range, phys_pages, page_size, flags)
    }

    /// Registers an already-mapped external contiguous physical range.
    pub fn register_external_range(
        &mut self,
        full_range: VirtAddrRange,
        mapped_range: VirtAddrRange,
        physical_range: PhysAddrRange,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        Self::validate_range(full_range, page_size)?;
        Self::validate_range(mapped_range, page_size)?;
        Self::validate_phys_range(physical_range, page_size)?;
        if !full_range.contains_range(mapped_range) {
            return Err(VMAllocError::OutOfOuterRange);
        }
        if physical_range.size() != mapped_range.size() {
            return Err(VMAllocError::RangeSizeMismatch);
        }

        let page_count = physical_range.size() / page_size;
        let mut phys_pages = Vec::with_capacity(page_count);
        for index in 0..page_count {
            phys_pages.push(physical_range.start + index * page_size);
        }

        self.register_external_pages(
            full_range,
            mapped_range,
            phys_pages.into_boxed_slice(),
            flags,
        )
    }

    /// Registers already-mapped external physical pages.
    pub fn register_external_pages(
        &mut self,
        full_range: VirtAddrRange,
        mapped_range: VirtAddrRange,
        phys_pages: Box<[PhysAddr]>,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        Self::validate_range(full_range, page_size)?;
        Self::validate_range(mapped_range, page_size)?;
        VMAllocRange::validate_phys_pages(&phys_pages, page_size)?;
        if !full_range.contains_range(mapped_range) {
            return Err(VMAllocError::OutOfOuterRange);
        }
        if phys_pages.len() != mapped_range.size() / page_size {
            return Err(VMAllocError::RangeSizeMismatch);
        }

        self.ranges.try_insert(
            full_range,
            VMAllocRange::new_external(mapped_range, phys_pages, flags),
        )?;
        Ok(())
    }

    /// Unmaps a vmalloc subrange.
    #[expect(unused)]
    pub fn unmap(&mut self, range: VirtAddrRange) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        allocation.unmap(range, page_size)
    }

    /// Deallocates a vmalloc range and cleans up any mapped pages.
    pub fn dealloc_range(&mut self, addr: VirtAddr) -> Result<(), VMAllocError> {
        let page_size = self.page_size();
        let full_start = {
            let (full_range, allocation) = self
                .ranges
                .find_mut(addr)
                .ok_or(VMAllocError::NotAllocated)?;
            if allocation.non_guard_range().start != addr {
                return Err(VMAllocError::NotAllocated);
            }

            allocation.unmap(allocation.non_guard_range(), page_size)?;
            full_range.start
        };

        self.ranges
            .remove(full_start)
            .expect("vmalloc range disappeared during deallocation");
        Ok(())
    }

    /// Translates a vmalloc virtual address to its backing physical address.
    pub fn virt_to_phys(&self, addr: VirtAddr) -> Option<PhysAddr> {
        let (_, allocation) = self.ranges.find(addr)?;
        allocation.virt_to_phys(addr, self.page_size())
    }

    /// Returns the page size used by this allocator.
    #[inline]
    fn page_size(&self) -> usize {
        1usize << self.page_size_shift
    }

    /// Validates that a virtual range is non-empty and page aligned.
    fn validate_range(range: VirtAddrRange, page_size: usize) -> Result<(), VMAllocError> {
        if !range.start.is_aligned(page_size) || !range.end.is_aligned(page_size) {
            return Err(VMAllocError::RangeNotAligned);
        }

        if range.is_empty() {
            return Err(VMAllocError::EmptyRange);
        }

        Ok(())
    }

    /// Validates that a physical range is non-empty and page aligned.
    fn validate_phys_range(range: PhysAddrRange, page_size: usize) -> Result<(), VMAllocError> {
        if !range.start.is_aligned(page_size) || !range.end.is_aligned(page_size) {
            return Err(VMAllocError::RangeNotAligned);
        }

        if range.is_empty() {
            return Err(VMAllocError::EmptyRange);
        }

        Ok(())
    }
}

/// Global vmalloc allocator.
static VMALLOC: LazyInit<SpinNoIrq<VMAllocator>> = LazyInit::new();

/// Initializes the global vmalloc allocator.
pub fn init_vmalloc(vmalloc_range: VirtAddrRange, page_size_shift: usize) {
    info!("Initializing VMAllocator...");
    VMALLOC.init_once(SpinNoIrq::new(
        VMAllocator::new(vmalloc_range, page_size_shift).unwrap(),
    ));
}

/// Runs a function with the global vmalloc allocator locked.
pub fn with_allocator<R>(f: impl FnOnce(&mut VMAllocator) -> R) -> Result<R, VMAllocError> {
    let mut allocator = VMALLOC.get().ok_or(VMAllocError::NotInitialized)?.lock();
    Ok(f(&mut allocator))
}

/// Allocates an unmapped global vmalloc range.
#[expect(unused)]
pub fn alloc_range(page_count: usize, guard_pages: usize) -> Result<VirtAddrRange, VMAllocError> {
    with_allocator(|allocator| allocator.alloc_range(page_count, guard_pages))?
}

/// Allocates and maps a global vmalloc range to owned pages.
pub fn alloc_range_and_map_alloc(
    page_count: usize,
    guard_pages: usize,
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    with_allocator(|allocator| allocator.alloc_range_and_map_alloc(page_count, guard_pages, flags))?
}

/// Allocates and maps a global vmalloc range to external contiguous pages.
#[expect(unused)]
pub fn alloc_range_and_map_phys_range(
    guard_pages: usize,
    physical_range: PhysAddrRange,
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    with_allocator(|allocator| {
        allocator.alloc_range_and_map_phys_range(guard_pages, physical_range, flags)
    })?
}

/// Allocates and maps a global vmalloc range to external page list entries.
#[expect(unused)]
pub fn alloc_range_and_map_phys_pages(
    guard_pages: usize,
    phys_pages: &[PhysAddr],
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    with_allocator(|allocator| {
        allocator.alloc_range_and_map_phys_pages(guard_pages, phys_pages, flags)
    })?
}

/// Maps owned pages into a global vmalloc subrange.
#[expect(unused)]
pub fn map_alloc(range: VirtAddrRange, flags: MappingFlags) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.map_alloc(range, flags))?
}

/// Maps an external contiguous physical range into a global vmalloc subrange.
#[expect(unused)]
pub fn map_phys_range(
    range: VirtAddrRange,
    physical_range: PhysAddrRange,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.map_phys_range(range, physical_range, flags))?
}

/// Maps external physical pages into a global vmalloc subrange.
#[expect(unused)]
pub fn map_phys_pages(
    range: VirtAddrRange,
    phys_pages: &[PhysAddr],
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.map_phys_pages(range, phys_pages, flags))?
}

/// Registers an already-mapped external contiguous physical range.
pub fn register_external_range(
    full_range: VirtAddrRange,
    mapped_range: VirtAddrRange,
    physical_range: PhysAddrRange,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| {
        allocator.register_external_range(full_range, mapped_range, physical_range, flags)
    })?
}

/// Registers already-mapped external physical pages.
#[expect(unused)]
pub fn register_external_pages(
    full_range: VirtAddrRange,
    mapped_range: VirtAddrRange,
    phys_pages: Box<[PhysAddr]>,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| {
        allocator.register_external_pages(full_range, mapped_range, phys_pages, flags)
    })?
}

/// Unmaps a global vmalloc subrange.
#[expect(unused)]
pub fn unmap(range: VirtAddrRange) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.unmap(range))?
}

/// Frees a global vmalloc range by its mapped start address.
#[expect(unused)]
pub fn dealloc_range(addr: VirtAddr) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.dealloc_range(addr))?
}

/// Translates a vmalloc virtual address to its backing physical address.
pub fn virt_to_phys(addr: VirtAddr) -> Option<PhysAddr> {
    VMALLOC.get()?.lock().virt_to_phys(addr)
}
