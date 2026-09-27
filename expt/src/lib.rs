//! Architecture-flexible page-table management.
//!
//! This crate builds and mutates hierarchical page tables described by
//! [`PageTableMeta`] and entries implementing [`GenericPTE`]. [`PageTable`] keeps
//! the concrete representation available for compile-time dispatch, while the
//! [`opaque`] module offers runtime selection among page-table formats.
//!
//! Mutations are performed through [`PageTableCursor`]. A cursor batches the TLB
//! invalidations required by mapping changes and performs them explicitly through
//! [`PageTableCursor::flush`] or automatically when it is dropped.

#![no_std]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(generic_const_items)]

/// The standard library used by unit tests.
///
/// Production builds remain `no_std`, while the test module uses host allocation
/// and thread-local storage.
#[cfg(test)]
extern crate std;

use core::marker::PhantomData;

use expalloc_trait::{DynPageAllocator, PageAllocator};
use maybe_non_generic::maybe_non_generic;
use memory_addr::{AddrRange, MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

pub mod arch;
mod meta;
pub mod opaque;
pub mod pte {
    //! Page-table entry types and flags.
    //!
    //! This module re-exports the entry abstractions supplied by the
    //! [`page_table_entry`] crate.
    pub use page_table_entry::*;
}
pub mod error;
mod flush;

pub use error::{PagingError, PagingResult};
pub(crate) use flush::{PendingTlbFlushes, TlbFlush};
pub use meta::{PageTableCoverage, PageTableMeta, VirtAddr};

/// A hierarchical page table with architecture-defined geometry.
///
/// `M` describes the virtual-address layout and TLB operations, while `PTE`
/// supplies the concrete entry representation. The value owns no allocation by
/// itself and identifies its root through a physical address.
pub struct PageTable<M: PageTableMeta, PTE: GenericPTE> {
    /// The physical address of the root page table.
    ///
    /// The table is accessed through the [`PageAllocator`] supplied to each
    /// allocating or mutating operation.
    root: PhysAddr,
    /// The compile-time association with the metadata and entry types.
    ///
    /// Neither type requires runtime storage in a page-table handle.
    _phantom: PhantomData<(PTE, M)>,
}

/// A mutating cursor over a page table.
///
/// Cursor operations record the virtual mappings they affect and flush the
/// local TLB when [`Self::flush`] is called or when the cursor is dropped.
pub struct PageTableCursor<'a, M: PageTableMeta, PTE: GenericPTE>
where
    [(); M::LEVELS]: Sized,
{
    /// The page table being mutated.
    ///
    /// The mutable borrow ensures that only one cursor can update the table at a
    /// time.
    table: &'a mut PageTable<M, PTE>,
    /// The TLB invalidations accumulated by cursor operations.
    ///
    /// These records are consumed by [`Self::flush`] or [`Drop::drop`].
    pending_flushes: PendingTlbFlushes<M>,
}

/// Computes the end of a page without exceeding a range.
///
/// Returns [`None`] when adding `page_size` overflows or when the resulting
/// address lies after `end`.
fn checked_page_end<A: MemoryAddr>(start: A, end: A, page_size: usize) -> Option<A> {
    let next = usize::checked_add(start.into(), page_size)?;
    if next <= end.into() {
        Some(A::from(next))
    } else {
        None
    }
}

/// Virtual address related constants and utility functions.
impl<M: PageTableMeta, PTE: GenericPTE> PageTable<M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// The shift that moves valid virtual-address bits to the most significant
    /// positions.
    const VA_SHIFT: usize = usize::BITS as usize - M::VA_BITS;
    /// The mask to extract the valid virtual address bits.
    const VA_VALID_MASK: usize = usize::MAX >> Self::VA_SHIFT;
    /// The mask that extracts virtual-address bits outside the valid range.
    const VA_HIGH_BITS_MASK: usize = !Self::VA_VALID_MASK;
    /// The mask to extract the most significant bit of the valid virtual address range.
    const VA_VALID_MSB_MASK: usize = 1usize << (M::VA_BITS - 1);

    /// Validates whether a virtual address is within the supported range and truncates it to
    /// extracts the valid portion according to the page table's coverage.
    pub(crate) const fn validate_and_truncate_vaddr(vaddr_raw: usize) -> Result<usize, ()> {
        let non_canonical = match M::COVERAGE {
            PageTableCoverage::Lower => (vaddr_raw & Self::VA_HIGH_BITS_MASK) != 0,
            PageTableCoverage::Upper => ((!vaddr_raw) & Self::VA_HIGH_BITS_MASK) != 0,
            PageTableCoverage::Symmetric => {
                let extended_vaddr =
                    ((vaddr_raw << Self::VA_SHIFT) as isize >> Self::VA_SHIFT) as usize;
                extended_vaddr != vaddr_raw
            }
        };

        if non_canonical {
            Err(())
        } else {
            Ok(vaddr_raw & Self::VA_VALID_MASK)
        }
    }

    /// Validates and truncates a range of virtual addresses according to the page table's coverage.
    pub(crate) fn validate_and_truncate_vaddr_range(
        range: AddrRange<M::VirtAddr>,
    ) -> PagingResult<M::VirtAddr, (usize, usize)> {
        // Validate and truncate the start virtual address.
        let start_truncated = Self::validate_and_truncate_vaddr(range.start.into())
            .map_err(|_| PagingError::NonCanonical { vaddr: range.start })?;

        // Validate and truncate the end virtual address. It's more complex as
        // `vaddr_end` is exclusive, so we need to handle it carefully:
        //
        // If `vaddr_end` is zero, we simply set the end to zero, unless the
        // coverage is not upper, where 0 is non-canonical and therefore the
        // end is considered non-canonical.
        //
        // If `vaddr_end` is not zero, we validate and truncate `vaddr_end - 1`
        // and then add 1 to get the exclusive end.
        let end_raw = range.end.into();

        if end_raw == 0 {
            return if matches!(M::COVERAGE, PageTableCoverage::Upper) {
                Err(PagingError::NonCanonical { vaddr: range.end })
            } else {
                Ok((start_truncated, 0))
            };
        }

        let end_inc_truncated = Self::validate_and_truncate_vaddr(end_raw - 1)
            .map_err(|_| PagingError::NonCanonical { vaddr: range.end })?;

        // If both start and end are valid canonical addresses in symmetric coverage, we need to
        // ensure they have the same sign extension. The exclusive end itself can be the first
        // address in the opposite half, so classify the last address in the range instead.
        if matches!(M::COVERAGE, PageTableCoverage::Symmetric)
            && (start_truncated ^ end_inc_truncated) & Self::VA_VALID_MSB_MASK != 0
        {
            return Err(PagingError::NonCanonical {
                // Self::VA_VALID_MSB_MASK equals to the first non-canonical address in the virtual
                // address space.
                vaddr: Self::VA_VALID_MSB_MASK.into(),
            });
        }

        Ok((start_truncated, end_inc_truncated + 1))
    }
}

impl<M: PageTableMeta, PTE: GenericPTE> PageTable<M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// Creates a page-table handle for an existing root.
    ///
    /// This function does not inspect or initialize the root table. Later
    /// operations access it through their selected [`PageAllocator`].
    ///
    /// # Safety
    ///
    /// The caller must ensure that the physical address is valid.
    pub unsafe fn new_at(paddr: PhysAddr) -> Self {
        #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
        let _ = meta::PageTableMetaAssertions::<M>::ASSERTIONS;

        Self {
            root: paddr,
            _phantom: PhantomData,
        }
    }

    /// Allocates and initializes a new root page table through `H`.
    ///
    /// The root has the size required by the highest level in `M` and is
    /// zero-initialized before the handle is returned.
    #[maybe_non_generic(
        new_alloc_dyn,
        type(H => handler: &DynPageAllocator),
        fn(Self::alloc_table => Self::alloc_table_dyn)
    )]
    pub fn new_alloc<H: PageAllocator>() -> PagingResult<M::VirtAddr, Self>
    where
        [(); M::LEVELS - 1]: Sized,
    {
        let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
        Ok(unsafe { Self::new_at(paddr) })
    }

    /// Returns the physical address of the root page table.
    pub const fn root_paddr(&self) -> PhysAddr {
        self.root
    }

    /// Creates a cursor for batched page-table mutations.
    ///
    /// All mapping changes should go through the returned cursor so affected
    /// virtual mappings can be collected and flushed together.
    pub fn cursor(&mut self) -> PageTableCursor<'_, M, PTE> {
        PageTableCursor {
            table: self,
            pending_flushes: PendingTlbFlushes::None,
        }
    }

    /// Returns the byte size of a page table at one level.
    ///
    /// The size is the concrete entry size multiplied by the number of entries
    /// configured for `LEVEL`.
    const fn table_size<const LEVEL: usize>() -> usize {
        size_of::<PTE>() << M::LEVEL_BITS[LEVEL]
    }

    /// Extracts the page-table index for one virtual-address level.
    ///
    /// `LEVEL` selects the bit range described by
    /// [`PageTableMeta::LEVEL_BIT_RANGES`].
    const fn index<const LEVEL: usize>(vaddr_truncated: usize) -> usize {
        let (start, end) = M::LEVEL_BIT_RANGES[LEVEL];

        (vaddr_truncated >> start) & ((1 << (end - start)) - 1)
    }

    /// Gets a mutable table at `LEVEL` from its physical address.
    #[maybe_non_generic(table_of_mut_non_const, const(LEVEL => level: usize))]
    #[maybe_non_generic(table_of_mut_dyn, type(H => handler: &DynPageAllocator))]
    #[maybe_non_generic(
        table_of_mut_non_const_dyn,
        const(LEVEL => level: usize),
        type(H => handler: &DynPageAllocator),
    )]
    fn table_of_mut<'a, const LEVEL: usize, H: PageAllocator>(paddr: PhysAddr) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];

        unsafe {
            let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }

    /// Allocates and clears a page table at one level.
    #[maybe_non_generic(alloc_table_dyn, type(H => handler: &DynPageAllocator))]
    fn alloc_table<const LEVEL: usize, H: PageAllocator>() -> PagingResult<M::VirtAddr, PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = H::alloc_frames_of_size(bytes_required) {
            let vaddr = H::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }
}

impl<M: PageTableMeta, PTE: GenericPTE> PageTableCursor<'_, M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// Flushes all TLB entries affected by changes made through this cursor.
    ///
    /// Calling this method clears the pending flush set, so the cursor's
    /// eventual [`Drop`] will not flush the same records again unless later
    /// operations add new pending flushes.
    pub fn flush(&mut self) {
        self.pending_flushes.flush();
    }

    /// Locates and clears an entry at a requested level.
    ///
    /// Missing intermediate tables may be allocated, and huge pages may be split,
    /// according to `create_if_not_exists` and `split_huge_page`. The returned
    /// index identifies the entry within its containing table.
    #[maybe_non_generic(
        get_page_entry_mut_dyn,
        type(H => handler: &DynPageAllocator),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(self.next_table_mut => self.next_table_mut_dyn),
        fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn),
    )]
    fn get_page_entry_mut<H: PageAllocator>(
        &mut self,
        vaddr_truncated: usize,
        vaddr: M::VirtAddr,
        level: usize,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<M::VirtAddr, (&mut PTE, usize)> {
        if level > M::MAX_PAGE_LEVEL {
            return Err(PagingError::CannotBePage { level });
        }

        // We use 0-indexed level here, so the naming is different from other
        // kernels. PML5 in other kernels is p4 in ours, PML4 is p3, etc.
        let p0 = if M::LEVELS > 1 {
            let p1 = if M::LEVELS > 2 {
                let p2 = if M::LEVELS > 3 {
                    let p3 = if M::LEVELS > 4 {
                        let p4 = PageTable::<M, PTE>::table_of_mut::<4, H>(self.table.root);
                        let index4 = PageTable::<M, PTE>::index::<4>(vaddr_truncated);
                        let p4e = &mut p4[index4];

                        if level == 4 {
                            self.clear_pte::<H>(p4e, 4, vaddr)?;
                            return Ok((p4e, index4));
                        }

                        self.next_table_mut::<3, H>(p4e, create_if_not_exists, split_huge_page)?
                    } else {
                        PageTable::<M, PTE>::table_of_mut::<3, H>(self.table.root)
                    };
                    let index3 = PageTable::<M, PTE>::index::<3>(vaddr_truncated);
                    let p3e = &mut p3[index3];

                    if level == 3 {
                        self.clear_pte::<H>(p3e, 3, vaddr)?;
                        return Ok((p3e, index3));
                    }

                    self.next_table_mut::<2, H>(p3e, create_if_not_exists, split_huge_page)?
                } else {
                    PageTable::<M, PTE>::table_of_mut::<2, H>(self.table.root)
                };
                let index2 = PageTable::<M, PTE>::index::<2>(vaddr_truncated);
                let p2e = &mut p2[index2];

                if level == 2 {
                    self.clear_pte::<H>(p2e, 2, vaddr)?;
                    return Ok((p2e, index2));
                }

                self.next_table_mut::<1, H>(p2e, create_if_not_exists, split_huge_page)?
            } else {
                PageTable::<M, PTE>::table_of_mut::<1, H>(self.table.root)
            };
            let index1 = PageTable::<M, PTE>::index::<1>(vaddr_truncated);
            let p1e = &mut p1[index1];

            if level == 1 {
                self.clear_pte::<H>(p1e, 1, vaddr)?;
                return Ok((p1e, index1));
            }

            self.next_table_mut::<0, H>(p1e, create_if_not_exists, split_huge_page)?
        } else {
            PageTable::<M, PTE>::table_of_mut::<0, H>(self.table.root)
        };
        let index0 = PageTable::<M, PTE>::index::<0>(vaddr_truncated);
        let p0e = &mut p0[index0];

        self.clear_pte::<H>(p0e, 0, vaddr)?;
        Ok((p0e, index0))
    }

    /// Gets a mutable child table at `LEVEL` from an entry at the level above.
    ///
    /// An unused entry can be populated with a newly allocated table. A huge leaf
    /// can instead be expanded into a child table whose entries preserve the old
    /// physical mapping and flags.
    #[maybe_non_generic(
        next_table_mut_dyn,
        type(H => handler: &DynPageAllocator),
        fn(PageTable::alloc_table => PageTable::alloc_table_dyn),
        fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn),
    )]
    fn next_table_mut<'a, const LEVEL: usize, H: PageAllocator>(
        &mut self,
        entry: &mut PTE,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<M::VirtAddr, &'a mut [PTE]> {
        if entry.is_unused() {
            if create_if_not_exists {
                let table = PageTable::<M, PTE>::alloc_table::<LEVEL, H>()?;
                *entry = GenericPTE::new_table(table);
                Ok(PageTable::<M, PTE>::table_of_mut::<LEVEL, H>(table))
            } else {
                Err(PagingError::NotMapped)
            }
        } else if entry.is_huge() {
            if split_huge_page {
                let old_paddr = entry.paddr();
                let flags = entry.flags();

                let table_paddr = PageTable::<M, PTE>::alloc_table::<LEVEL, H>()?;
                *entry = GenericPTE::new_table(table_paddr);

                // A huge-page TLB entry may cover any address in the old mapping.
                // Use a conservative full flush when replacing a huge leaf with a table.
                self.pending_flushes += TlbFlush::Full;

                let table = PageTable::<M, PTE>::table_of_mut::<LEVEL, H>(table_paddr);
                for (i, entry) in table.iter_mut().enumerate() {
                    *entry =
                        PTE::new_page(old_paddr + i * M::LEVEL_PAGE_SIZE[LEVEL], flags, LEVEL != 0);
                }

                Ok(table)
            } else {
                Err(PagingError::MappedToHugePage)
            }
        } else {
            Ok(PageTable::<M, PTE>::table_of_mut::<LEVEL, H>(entry.paddr()))
        }
    }

    /// Clears an entry and all leaf mappings below it.
    ///
    /// Clearing a leaf records the required TLB invalidation. For a table entry,
    /// the function recursively clears its descendants while retaining all table
    /// allocations.
    #[maybe_non_generic(
        clear_pte_dyn,
        type(H => handler: &DynPageAllocator),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(PageTable::table_of_mut_non_const => PageTable::table_of_mut_non_const_dyn),
    )]
    fn clear_pte<H: PageAllocator>(
        &mut self,
        entry: &mut PTE,
        level: usize,
        vaddr: M::VirtAddr,
    ) -> PagingResult<M::VirtAddr> {
        if entry.is_unused() {
            Ok(())
        } else if level == 0 || entry.is_huge() {
            if entry.is_huge() {
                self.pending_flushes += TlbFlush::Full;
            } else {
                self.pending_flushes += TlbFlush::Page(vaddr);
            }
            entry.clear();
            Ok(())
        } else {
            let table = PageTable::<M, PTE>::table_of_mut_non_const::<H>(entry.paddr(), level - 1);
            for (index, child) in table.iter_mut().enumerate() {
                let child_vaddr = vaddr + index * M::LEVEL_PAGE_SIZE[level - 1];
                self.clear_pte::<H>(child, level - 1, child_vaddr)?;
            }
            Ok(())
        }
    }

    /// Splits a range into the fewest largest supported pages and invokes a
    /// callback for each selected page.
    ///
    /// The function removes current mappings from the range and calls `f` for the
    /// entry representing each selected page.
    ///
    /// The callback parameters are the entry level, its table index, the page's
    /// starting virtual address, and a mutable reference to the entry. Level `0`
    /// is the lowest level and `M::LEVELS - 1` is the highest.
    ///
    /// Before calling the function, the PTE is [cleared](GenericPTE::clear). If
    /// the PTE points to a table, descendant entries are cleared recursively and
    /// table pages are retained. If the range overlaps with a huge page, the
    /// page is split into smaller pages.
    ///
    /// The range is aligned to the page size of the lowest level.
    #[maybe_non_generic(
        iter_pages_in_range_dyn,
        type(H => handler: &DynPageAllocator),
        fn(self.get_page_entry_mut => self.get_page_entry_mut_dyn),
    )]
    fn iter_pages_in_range<F, H: PageAllocator>(
        &mut self,
        range: AddrRange<M::VirtAddr>,
        mut f: F,
    ) -> PagingResult<M::VirtAddr>
    where
        F: FnMut(usize, usize, M::VirtAddr, &mut PTE) -> PagingResult<M::VirtAddr, TlbFlush<M>>,
    {
        let range_aligned = AddrRange {
            start: range.start.align_down(M::LEVEL_PAGE_SIZE[0]),
            end: range.end.align_up(M::LEVEL_PAGE_SIZE[0]),
        };
        let range_truncated =
            PageTable::<M, PTE>::validate_and_truncate_vaddr_range(range_aligned)?;

        let (mut start_truncated, end_truncated) = range_truncated;
        let mut start_vaddr = range_aligned.start;

        while start_truncated < end_truncated {
            for level in (0..=M::MAX_PAGE_LEVEL).rev() {
                let page_size = M::LEVEL_PAGE_SIZE[level];
                if start_truncated.is_aligned(page_size) {
                    let Some(next_truncated) =
                        checked_page_end(start_truncated, end_truncated, page_size)
                    else {
                        continue;
                    };

                    let flush = {
                        let (entry, index) = self.get_page_entry_mut::<H>(
                            start_truncated,
                            start_vaddr,
                            level,
                            true,
                            true,
                        )?;
                        f(level, index, start_vaddr, entry)?
                    };
                    self.pending_flushes += flush;
                    start_vaddr = start_vaddr + (next_truncated - start_truncated);
                    start_truncated = next_truncated;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Maps a virtual address range to a physical address range.
    ///
    /// The range is rounded to base-page boundaries, existing mappings in the
    /// covered range are replaced, and the virtual-to-physical offset is
    /// preserved across the mapped range.
    #[maybe_non_generic(
        map_dyn,
        type(H => handler: &DynPageAllocator),
        fn(self.iter_pages_in_range => self.iter_pages_in_range_dyn),
    )]
    pub fn map<H: PageAllocator>(
        &mut self,
        vaddr: M::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<M::VirtAddr> {
        let offset = usize::wrapping_sub(vaddr.into(), paddr.into());
        self.iter_pages_in_range::<_, H>(
            AddrRange::new(vaddr, vaddr + size),
            |level, _index, page_vaddr, entry| {
                let page_paddr = PhysAddr::from_usize(page_vaddr.wrapping_sub(offset).into());
                *entry = GenericPTE::new_page(page_paddr, flags, level != 0);
                Ok(TlbFlush::Page(page_vaddr))
            },
        )
    }

    /// Removes mappings from a virtual address range.
    ///
    /// The range is rounded to base-page boundaries. Existing huge mappings may
    /// be split or cleared as needed, and any affected TLB entries are recorded
    /// for this cursor's next flush.
    #[maybe_non_generic(
        unmap_dyn,
        type(H => handler: &DynPageAllocator),
        fn(self.iter_pages_in_range => self.iter_pages_in_range_dyn),
    )]
    pub fn unmap<H: PageAllocator>(
        &mut self,
        vaddr: M::VirtAddr,
        size: usize,
    ) -> PagingResult<M::VirtAddr> {
        self.iter_pages_in_range::<_, H>(
            AddrRange::new(vaddr, vaddr + size),
            |_level, _index, _page_vaddr, _entry| Ok(TlbFlush::None),
        )
    }
}

impl<'a, M: PageTableMeta, PTE: GenericPTE> Drop for PageTableCursor<'a, M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// Flushes invalidations left pending when the cursor leaves scope.
    ///
    /// This guarantees that successfully applied mutations are synchronized even
    /// when a later cursor operation returns an error.
    fn drop(&mut self) {
        self.flush();
    }
}

/// Unit tests for concrete traversal, mapping, and TLB behavior.
///
/// The tests use a compact three-level format and an in-memory frame allocator
/// to exercise page-table state deterministically.
#[cfg(test)]
mod tests;
