//! Single-root multi-level page table implementation.
//!
//! This module provides [`PageTable`] and [`PageTableCursor`] for one concrete
//! [`crate::meta::PageTableMeta`] and [`page_table_entry::GenericPTE`] pair.

use core::{marker::PhantomData, mem::ManuallyDrop};

use expalloc_trait::{DynPageAllocator, PageAllocator};
use maybe_non_generic::maybe_non_generic;
use memory_addr::{AddrRange, AddrRangeBounds, MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::{PagingError, PagingResult},
    meta::{PageTableCoverage, PageTableMeta},
    pt::{
        PageTableCursorLike, PageTableLike,
        cleanup::{
            DeallocationItem, DeallocationList, PendingCleanup, TlbFlush, TlbFlusher, TlbFlushes,
        },
    },
};

/// A single-root multi-level page table.
///
/// `M` describes the virtual-address layout, `PTE` supplies
/// the concrete entry representation, and `AL` selects the allocator that is
/// used to allocate and deallocate table frames. `F` selects the [`TlbFlusher`] used by cursor
/// cleanup, including its local or remote invalidation scope. See that trait for strategy
/// selection and synchronization requirements.
///
/// The page table hierarchy must form a valid tree.
///
/// See the crate-level documentation for table-frame deallocation and hardware-use requirements.
///
/// Use [`PageTable::leak`] to consume this value while retaining its table frames, then use
/// [`PageTable::new_at`] to construct another value from the existing hierarchy.
pub struct PageTable<M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// The physical address of the root page table.
    ///
    /// The table is accessed through the selected [`PageAllocator`].
    root: PhysAddr,
    /// The marker for the metadata, entry, allocator, and TLB flusher types.
    ///
    /// These types determine the value's behavior without being stored as runtime fields.
    _phantom: PhantomData<(PTE, M, AL, F)>,
}

/// A mutating cursor over a [`PageTable`].
///
/// The mutable borrow excludes other Rust mutations through the borrowed value. Callers
/// remain responsible for coordinating hardware use with the selected [`TlbFlusher`],
/// and for ensuring that no other multi-level page table manages the same table frames.
///
/// See [`PageTableCursorLike`] for mutation and partial-failure behavior. [`PendingCleanup`]
/// performs invalidation through `F` before deallocating detached table frames through `AL`.
pub struct PageTableCursor<'a, M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// The [`PageTable`] being mutated.
    ///
    /// The mutable borrow lasts for the cursor's lifetime.
    pub(crate) table: &'a mut PageTable<M, PTE, AL, F>,
    /// The cleanup accumulated by cursor operations.
    ///
    /// See [`Self::take_cleanup`] for transferring records before synchronization.
    pending_cleanup: PendingCleanup<M::VirtAddr>,
}

/// Computes the end of a page without exceeding a range.
///
/// Returns [`None`] when adding `page_size` overflows or when the resulting
/// address lies after `end`.
pub(crate) fn checked_page_end<A: MemoryAddr>(start: A, end: A, page_size: usize) -> Option<A> {
    let next = usize::checked_add(start.into(), page_size)?;
    if next <= end.into() {
        Some(A::from(next))
    } else {
        None
    }
}

/// Returns the maximum page level whose corresponding page size is a factor of the offset.
///
/// This function is used to determine the maximum huge page size when mapping a virtual address
/// range, to make sure that both physical address and virtual address are aligned to the same page
/// size.
///
/// Zero offset is considered to be a multiple of any page size.
///
/// Returns [`None`] when the offset is invalid, i.e. not a multiple of any page size.
pub(crate) fn max_page_level_for_offset<M: PageTableMeta>(offset: usize) -> Option<usize>
where
    [(); M::LEVELS]: Sized,
{
    let mut level = M::MAX_PAGE_LEVEL;

    loop {
        let level_page_size = M::LEVEL_PAGE_SIZE[level];
        if offset.is_multiple_of(level_page_size) {
            return Some(level);
        }

        if level == 0 {
            return None;
        }

        level -= 1;
    }
}

/// Virtual-address validation utilities.
///
/// Validation uses the coverage policy selected by `M`.
impl<M, PTE, AL, F> PageTable<M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// Validates and truncates a virtual address range according to its metadata.
    ///
    /// The result contains the range endpoints with canonical extension removed.
    /// Ranges outside the metadata's coverage are rejected.
    pub const fn validate_and_truncate_vaddr_range(
        range: AddrRange<M::VirtAddr>,
    ) -> PagingResult<M::VirtAddr, (usize, usize)> {
        <M::Coverage as PageTableCoverage>::validate_and_truncate_vaddr_range(range, M::VA_BITS)
            .map_err(const |vaddr| PagingError::NonCanonical {
                vaddr: M::VirtAddr::from(vaddr),
            })
    }
}

/// Multi-level page table construction and access to hierarchy table frames.
///
/// All table frame allocations and physical-address translations use `AL`.
impl<M, PTE, AL, F> PageTable<M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// Constructs a multi-level page table from an existing page table hierarchy.
    ///
    /// This function does not inspect or initialize the root page table. Later
    /// operations access it through `AL`.
    ///
    /// # Safety
    ///
    /// The root must identify a valid tree-shaped page table hierarchy for `M` and `PTE`.
    ///
    /// No other code may access the page table hierarchy while the [`PageTable`]
    /// lives.
    ///
    /// The [`PageAllocator`] specified by `AL` must be able to manage the existing
    /// table frames.
    pub unsafe fn new_at(paddr: PhysAddr) -> Self {
        #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
        let _ = crate::meta::PageTableMetaAssertions::<M>::ASSERTIONS;

        Self {
            root: paddr,
            _phantom: PhantomData,
        }
    }

    /// Allocates and initializes a new [`PageTable`] through `AL`.
    ///
    /// This function allocates only the root level page table and zero-initializes it.
    pub fn new_alloc() -> PagingResult<M::VirtAddr, Self> {
        let paddr = Self::alloc_table::<{ M::LEVELS - 1 }>()?;
        Ok(unsafe { Self::new_at(paddr) })
    }

    /// Returns the root physical address without changing the table frames' lifetime.
    ///
    /// Dropping the [`PageTable`] deallocates its table frames,
    /// so the caller must not use this address after dropping the [`PageTable`].
    pub const fn root(&self) -> PhysAddr {
        self.root
    }

    /// Consumes the [`PageTable`] without deallocating its table frames.
    ///
    /// Another [`PageTable`] can use the root if its allocator can access and
    /// deallocate every table frame in the page table hierarchy. Otherwise, the caller must
    /// arrange their deallocation. The allocator type may differ from `AL` if those
    /// requirements hold. The returned address remains usable until the retained
    /// table frames are deallocated, either by the caller or by a value that adopts them.
    pub fn leak(self) -> PhysAddr {
        let manually_dropped = ManuallyDrop::new(self);
        manually_dropped.root
    }

    /// Creates a cursor for batched page table hierarchy mutations.
    ///
    /// Mapping changes accumulate [`PendingCleanup`]. See [`PageTableCursor`] for batching and
    /// synchronization requirements.
    pub fn cursor(&mut self) -> PageTableCursor<'_, M, PTE, AL, F> {
        PageTableCursor {
            table: self,
            pending_cleanup: PendingCleanup::new_with::<AL, F>(),
        }
    }

    /// Returns the byte size of a page table at one level.
    ///
    /// The size is the concrete entry size multiplied by the number of entries
    /// configured for `LEVEL`.
    #[maybe_non_generic(table_size_non_const, const(LEVEL => level: usize))]
    const fn table_size<const LEVEL: usize>() -> usize {
        size_of::<PTE>() << M::LEVEL_BITS[LEVEL]
    }

    /// Extracts the page table index for one virtual-address level.
    ///
    /// `LEVEL` selects the bit range described by
    /// [`PageTableMeta::LEVEL_BIT_RANGES`].
    const fn index<const LEVEL: usize>(vaddr_truncated: usize) -> usize {
        let (start, end) = M::LEVEL_BIT_RANGES[LEVEL];

        (vaddr_truncated >> start) & ((1 << (end - start)) - 1)
    }

    /// Returns a mutable slice of page table entries at `LEVEL` from its physical address.
    ///
    /// The allocator translates the address into a slice containing the entry
    /// count configured by the metadata.
    #[maybe_non_generic(table_of_mut_non_const, const(LEVEL => level: usize))]
    fn table_of_mut<'a, const LEVEL: usize>(paddr: PhysAddr) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];

        unsafe {
            let ptr: *mut PTE = AL::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }

    /// Allocates and clears a page table at one level.
    ///
    /// The allocation uses the table's byte size, and every entry is zeroed
    /// before the physical address is returned.
    fn alloc_table<const LEVEL: usize>() -> PagingResult<M::VirtAddr, PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = AL::alloc_frames_of_size(bytes_required) {
            let vaddr = AL::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }
}

/// Table-frame deallocation.
///
/// See the crate-level documentation for the deallocated frames and hardware-use requirements.
impl<M, PTE, AL, F> Drop for PageTable<M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// Deallocates the page table hierarchy's table frames.
    ///
    /// Child page tables are visited before their parents, and mapped data frames are retained.
    /// Destruction does not request TLB invalidation.
    fn drop(&mut self) {
        // Recursively visit the page table hierarchy and deallocate its table frames.
        let mut cursor = self.cursor();

        cursor
            .iter_existing_page_tables(&mut |level, paddr| {
                AL::dealloc_frames_of_size(paddr, Self::table_size_non_const(level));

                Ok(())
            })
            .unwrap();

        drop(cursor);
    }
}

/// Cursor traversal and mutation operations.
///
/// Mutations accumulate [`PendingCleanup`] until it is processed or transferred to the caller.
impl<M, PTE, AL, F> PageTableCursor<'_, M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// Processes all pending cleanup.
    ///
    /// Takes the current batch and calls [`PendingCleanup::clean_up`]. Later mutations accumulate
    /// new cleanup records.
    pub fn clean_up(&mut self) {
        // Safety: we'll handle it immediately after calling this function.
        unsafe { self.take_cleanup() }.clean_up();
    }

    /// Transfers pending cleanup to the caller and empties this cursor's records.
    ///
    /// # Safety
    ///
    /// The caller must handle the returned cleanup properly after calling this
    /// function, and before any other operations on the cursor.
    pub unsafe fn take_cleanup(&mut self) -> PendingCleanup<M::VirtAddr> {
        core::mem::replace(
            &mut self.pending_cleanup,
            PendingCleanup::new_with::<AL, F>(),
        )
    }

    /// Locates and clears an entry at a requested level.
    ///
    /// Missing intermediate tables may be allocated, and huge pages may be split,
    /// according to `create_if_not_exists` and `split_huge_page`. The returned
    /// index identifies the entry within its containing table.
    fn get_page_entry_mut(
        &mut self,
        vaddr_truncated: usize,
        vaddr: M::VirtAddr,
        level: usize,
        create_if_not_exists: bool,
        split_huge_page: bool,
        replace_table: bool,
    ) -> PagingResult<M::VirtAddr, (&mut PTE, usize)> {
        if level > M::MAX_PAGE_LEVEL {
            return Err(PagingError::CannotBePage { level });
        }

        // Levels are numbered upward from zero at the lowest page table. On x86-64,
        // p0 is the PT, p1 is the PD, p2 is the PDPT, p3 is the PML4, and p4 is the PML5.
        let p0 = if M::LEVELS > 1 {
            let p1 = if M::LEVELS > 2 {
                let p2 = if M::LEVELS > 3 {
                    let p3 = if M::LEVELS > 4 {
                        let p4 = PageTable::<M, PTE, AL, F>::table_of_mut::<4>(self.table.root);
                        let index4 = PageTable::<M, PTE, AL, F>::index::<4>(vaddr_truncated);
                        let p4e = &mut p4[index4];

                        if level == 4 {
                            self.clear_pte(p4e, 4, vaddr, replace_table)?;
                            return Ok((p4e, index4));
                        }

                        self.next_table_mut::<3>(p4e, create_if_not_exists, split_huge_page)?
                    } else {
                        PageTable::<M, PTE, AL, F>::table_of_mut::<3>(self.table.root)
                    };
                    let index3 = PageTable::<M, PTE, AL, F>::index::<3>(vaddr_truncated);
                    let p3e = &mut p3[index3];

                    if level == 3 {
                        self.clear_pte(p3e, 3, vaddr, replace_table)?;
                        return Ok((p3e, index3));
                    }

                    self.next_table_mut::<2>(p3e, create_if_not_exists, split_huge_page)?
                } else {
                    PageTable::<M, PTE, AL, F>::table_of_mut::<2>(self.table.root)
                };
                let index2 = PageTable::<M, PTE, AL, F>::index::<2>(vaddr_truncated);
                let p2e = &mut p2[index2];

                if level == 2 {
                    self.clear_pte(p2e, 2, vaddr, replace_table)?;
                    return Ok((p2e, index2));
                }

                self.next_table_mut::<1>(p2e, create_if_not_exists, split_huge_page)?
            } else {
                PageTable::<M, PTE, AL, F>::table_of_mut::<1>(self.table.root)
            };
            let index1 = PageTable::<M, PTE, AL, F>::index::<1>(vaddr_truncated);
            let p1e = &mut p1[index1];

            if level == 1 {
                self.clear_pte(p1e, 1, vaddr, replace_table)?;
                return Ok((p1e, index1));
            }

            self.next_table_mut::<0>(p1e, create_if_not_exists, split_huge_page)?
        } else {
            PageTable::<M, PTE, AL, F>::table_of_mut::<0>(self.table.root)
        };
        let index0 = PageTable::<M, PTE, AL, F>::index::<0>(vaddr_truncated);
        let p0e = &mut p0[index0];

        self.clear_pte(p0e, 0, vaddr, replace_table)?;
        Ok((p0e, index0))
    }

    /// Gets a mutable child page table at `LEVEL` from an entry at the level above.
    ///
    /// An unused entry can be populated with a newly allocated table. A huge leaf
    /// can instead be expanded into a child table whose entries preserve the old
    /// physical mapping and flags.
    fn next_table_mut<'a, const LEVEL: usize>(
        &mut self,
        entry: &mut PTE,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<M::VirtAddr, &'a mut [PTE]> {
        if entry.is_unused() {
            if create_if_not_exists {
                let table = PageTable::<M, PTE, AL, F>::alloc_table::<LEVEL>()?;
                *entry = GenericPTE::new_table(table);
                // A new non-leaf descriptor changes the walk topology. RISC-V address-specific
                // SFENCE.VMA covers leaf translations only, so conservatively request all-address
                // invalidation through the selected strategy.
                self.pending_cleanup.push_flush(TlbFlush::Full);
                Ok(PageTable::<M, PTE, AL, F>::table_of_mut::<LEVEL>(table))
            } else {
                Err(PagingError::NotMapped)
            }
        } else if entry.is_huge() {
            if split_huge_page {
                let old_paddr = entry.paddr();
                let flags = entry.flags();

                let table_paddr = PageTable::<M, PTE, AL, F>::alloc_table::<LEVEL>()?;
                *entry = GenericPTE::new_table(table_paddr);

                // A huge-page TLB entry may cover any address in the old mapping.
                // Use a conservative full flush when replacing a huge leaf with a table.
                self.pending_cleanup.push_flush(TlbFlush::Full);

                let table = PageTable::<M, PTE, AL, F>::table_of_mut::<LEVEL>(table_paddr);
                for (i, entry) in table.iter_mut().enumerate() {
                    *entry =
                        PTE::new_page(old_paddr + i * M::LEVEL_PAGE_SIZE[LEVEL], flags, LEVEL != 0);
                }

                Ok(table)
            } else {
                Err(PagingError::MappedToHugePage)
            }
        } else {
            Ok(PageTable::<M, PTE, AL, F>::table_of_mut::<LEVEL>(
                entry.paddr(),
            ))
        }
    }

    /// Clears an entry or its descendant leaf mappings.
    ///
    /// Clearing a leaf records the required TLB invalidation. When replacing a table entry, all
    /// descendant table allocations are recorded before clearing the descriptor. Otherwise only
    /// descendant leaves are cleared, retaining table descriptors and their allocations.
    /// If recording allocations fails, the descriptor and its subtree remain unchanged and only
    /// temporary record storage is deallocated.
    fn clear_pte(
        &mut self,
        entry: &mut PTE,
        level: usize,
        vaddr: M::VirtAddr,
        replace_table: bool,
    ) -> PagingResult<M::VirtAddr> {
        if entry.is_unused() {
            Ok(())
        } else if level == 0 || entry.is_huge() {
            self.pending_cleanup.push_flush(if entry.is_huge() {
                TlbFlush::Full
            } else {
                TlbFlush::Page(vaddr)
            });
            entry.clear();
            Ok(())
        } else if replace_table {
            let mut records = DeallocationList::new(&DynPageAllocator::new::<AL>());

            // Collect all frames in this subtree and record them for deallocation.
            Self::iter_existing_page_tables_inner(
                level - 1,
                entry.paddr(),
                PageTable::<M, PTE, AL, F>::table_of_mut_non_const(entry.paddr(), level - 1),
                &mut |level, paddr| {
                    records
                        .push(DeallocationItem {
                            paddr,
                            size: PageTable::<M, PTE, AL, F>::table_size_non_const(level),
                        })
                        .map_err(|_| PagingError::AllocationFailed)
                },
            )?;

            entry.clear();
            self.pending_cleanup.append_flushes(TlbFlushes::Full);
            // SAFETY: they all use the same allocator.
            unsafe { self.pending_cleanup.append_deallocations(records) };
            Ok(())
        } else {
            let table =
                PageTable::<M, PTE, AL, F>::table_of_mut_non_const(entry.paddr(), level - 1);
            for (index, child) in table.iter_mut().enumerate() {
                let child_vaddr = vaddr + index * M::LEVEL_PAGE_SIZE[level - 1];
                self.clear_pte(child, level - 1, child_vaddr, false)?;
            }
            Ok(())
        }
    }

    /// Splits a range into the largest supported pages and invokes a callback for each.
    ///
    /// The function removes current mappings from the range and calls `c` for the
    /// entry representing each selected page.
    ///
    /// The callback parameters are the entry level, its table index, the page's
    /// starting virtual address, and a mutable reference to the entry. Level `0`
    /// is the lowest level and `M::LEVELS - 1` is the highest.
    ///
    /// Before invoking the callback, an existing leaf is [cleared](GenericPTE::clear). A child
    /// table is either disconnected after recording its allocations or recursively cleared with
    /// its table descriptors retained, according to the replacement argument. Huge mappings are
    /// split when traversal must descend to a lower level.
    ///
    /// The input range is expanded outward to the page size of the lowest level.
    ///
    /// `max_page_level` is the maximum supported page level, `None` means using the maximum page
    /// level specified by the metadata.
    fn iter_pages_in_range<C>(
        &mut self,
        range: AddrRange<M::VirtAddr>,
        max_page_level: Option<usize>,
        replace_table: bool,
        mut c: C,
    ) -> PagingResult<M::VirtAddr>
    where
        C: FnMut(
            usize,
            usize,
            M::VirtAddr,
            &mut PTE,
        ) -> PagingResult<M::VirtAddr, TlbFlush<M::VirtAddr>>,
    {
        let max_page_level = max_page_level
            .unwrap_or(M::MAX_PAGE_LEVEL)
            .min(M::MAX_PAGE_LEVEL);
        let range_aligned = range
            .align_outwards(M::LEVEL_PAGE_SIZE[0])
            .ok_or(PagingError::NonCanonical { vaddr: range.end })?;
        let range_truncated =
            PageTable::<M, PTE, AL, F>::validate_and_truncate_vaddr_range(range_aligned)?;

        let (mut start_truncated, end_truncated) = range_truncated;
        let mut start_vaddr = range_aligned.start;

        while start_truncated < end_truncated {
            for level in (0..=max_page_level).rev() {
                let page_size = M::LEVEL_PAGE_SIZE[level];
                if start_truncated.is_aligned(page_size) {
                    let Some(next_truncated) =
                        checked_page_end(start_truncated, end_truncated, page_size)
                    else {
                        continue;
                    };

                    let flush = {
                        let (entry, index) = self.get_page_entry_mut(
                            start_truncated,
                            start_vaddr,
                            level,
                            true,
                            true,
                            replace_table,
                        )?;
                        c(level, index, start_vaddr, entry)?
                    };
                    self.pending_cleanup
                        .append_flushes(TlbFlushes::None + flush);
                    start_vaddr = start_vaddr + (next_truncated - start_truncated);
                    start_truncated = next_truncated;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Invokes a callback for each page table in the hierarchy rooted here.
    ///
    /// The callback parameters are the level of the page table, and the physical
    /// address of the page table.
    ///
    /// This function performs a depth-first post-order traversal of the page table
    /// hierarchy. At the same level, page tables are visited from left to right.
    /// For example, a three-level page table hierarchy with two entries per page
    /// table has this layout:
    ///
    /// ```text
    /// Root page table (level 2):
    /// - Entry 0 -> Page table (level 1) 0:
    ///   - Entry 0 -> Page table (level 0) 0: Entries 0 and 1
    ///   - Entry 1 -> Page table (level 0) 1: Entries 0 and 1
    /// - Entry 1 -> Page table (level 1) 1:
    ///   - Entry 0 -> Page table (level 0) 2: Entries 0 and 1
    ///   - Entry 1 -> Page table (level 0) 3: Entries 0 and 1
    /// ```
    ///
    /// The suffixes identify page tables within each level. Entry indices are local to each table.
    ///
    /// The order will be:
    ///
    /// ```text
    /// Page table (level 0) 0
    /// Page table (level 0) 1
    /// Page table (level 1) 0
    /// Page table (level 0) 2
    /// Page table (level 0) 3
    /// Page table (level 1) 1
    /// Root page table (level 2)
    /// ```
    fn iter_existing_page_tables<C>(&mut self, c: &mut C) -> PagingResult<M::VirtAddr>
    where
        C: FnMut(usize, PhysAddr) -> PagingResult<M::VirtAddr>,
        [(); M::LEVELS - 1]: Sized,
    {
        let root_paddr = self.table.root;
        let root_table = PageTable::<M, PTE, AL, F>::table_of_mut::<{ M::LEVELS - 1 }>(root_paddr);
        Self::iter_existing_page_tables_inner(M::LEVELS - 1, root_paddr, root_table, c)
    }

    /// Visits a page table's descendants and then invokes its callback.
    ///
    /// The callback receives the level and physical address of each page table in post-order.
    fn iter_existing_page_tables_inner<C>(
        this_level: usize,
        this_paddr: PhysAddr,
        this_table: &mut [PTE],
        c: &mut C,
    ) -> PagingResult<M::VirtAddr>
    where
        C: FnMut(usize, PhysAddr) -> PagingResult<M::VirtAddr>,
    {
        // Above level zero, visit each child page table before visiting this page table.
        if this_level > 0 {
            for entry in this_table.iter_mut() {
                if entry.is_unused() || entry.is_huge() {
                    continue;
                }

                let next_paddr = entry.paddr();
                let next_table =
                    PageTable::<M, PTE, AL, F>::table_of_mut_non_const(next_paddr, this_level - 1);
                Self::iter_existing_page_tables_inner(this_level - 1, next_paddr, next_table, c)?;
            }
        }

        // The callback runs after this page table's descendants have been visited.
        // Known issue: a deallocation callback can invalidate `this_table` before this function
        // returns, violating Rust's reference-validity requirements. We just trust the Rust Compiler
        // not to complain about it.
        c(this_level, this_paddr)
    }

    /// Maps a virtual address range to a physical address range.
    ///
    /// The range is rounded outwards to base-page boundaries, existing mappings
    /// in the covered range are replaced, and the virtual-to-physical offset is
    /// preserved across the mapped range.
    ///
    /// The physical address is rounded down to base-page boundary.
    ///
    /// Mapping a range of zero size is a no-op.
    ///
    /// See [PageTableCursorLike::map] for cleanup and partial-failure behavior.
    pub fn map(
        &mut self,
        vaddr: M::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<M::VirtAddr> {
        if size == 0 {
            return Ok(());
        }

        let paddr = paddr.align_down(M::LEVEL_PAGE_SIZE[0]);
        let vaddr_aligned = vaddr.align_down(M::LEVEL_PAGE_SIZE[0]);
        let offset = usize::wrapping_sub(vaddr_aligned.into(), paddr.into());
        let end = vaddr
            .checked_add(size)
            .ok_or(PagingError::NonCanonical { vaddr })?;
        let max_page_level = max_page_level_for_offset::<M>(offset).unwrap_or_else(|| {
            unreachable!("offset must be a multiple of some page size");
        });
        self.iter_pages_in_range::<_>(
            AddrRange::new(vaddr, end),
            Some(max_page_level),
            true,
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
    /// for this cursor's next cleanup.
    ///
    /// Unmapping a range of zero size is no-op.
    ///
    /// Data frames are not deallocated. Intermediate tables remain allocated, and
    /// absent paths may allocate tables. An error can leave earlier changes
    /// applied. See [PageTableCursorLike::unmap] for cleanup behavior.
    pub fn unmap(&mut self, vaddr: M::VirtAddr, size: usize) -> PagingResult<M::VirtAddr> {
        if size == 0 {
            return Ok(());
        }

        let end = vaddr
            .checked_add(size)
            .ok_or(PagingError::NonCanonical { vaddr })?;
        self.iter_pages_in_range::<_>(
            AddrRange::new(vaddr, end),
            None,
            false,
            |_level, _index, _page_vaddr, _entry| Ok(TlbFlush::None),
        )
    }
}

/// The common interface for a single-root multi-level page table.
///
/// Operations delegate to the concrete methods with the same lifecycle contract.
impl<M, PTE, AL, F> PageTableLike for PageTable<M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// The virtual-address type configured by the metadata.
    ///
    /// Mapping operations use this semantic address representation.
    type VirtAddr = M::VirtAddr;
    /// The physical address of the single root.
    ///
    /// Copying this address does not change the hierarchy's table-frame lifetime.
    type Root = PhysAddr;
    /// The allocator for table frames.
    ///
    /// Table access, allocation, and deallocation use `AL`.
    type Allocator = AL;
    /// The TLB flusher for this page table hierarchy.
    ///
    /// TLB invalidations are performed using this type.
    type TlbFlusher = F;
    /// The cursor borrowing the multi-level page table.
    ///
    /// It uses the same metadata, entry, allocator, and TLB flusher types as the borrowed value.
    type Cursor<'a>
        = PageTableCursor<'a, M, PTE, AL, F>
    where
        Self: 'a;

    /// Constructs a single-root multi-level page table from an existing root.
    ///
    /// The caller must meet the safety requirements of [`PageTable::new_at`].
    unsafe fn new_at(root: Self::Root) -> Self {
        unsafe { Self::new_at(root) }
    }

    /// Allocates and initializes a new single-root multi-level page table.
    ///
    /// This delegates to [`PageTable::new_alloc`].
    fn new_alloc() -> PagingResult<Self::VirtAddr, Self> {
        Self::new_alloc()
    }

    /// Consumes the multi-level page table while retaining its table-frame storage.
    ///
    /// This delegates to [`PageTable::leak`] and returns the root for transfer.
    fn leak(self) -> Self::Root {
        self.leak()
    }

    /// Returns the root address without changing table-frame lifetime.
    ///
    /// This delegates to [`PageTable::root`].
    fn root(&self) -> Self::Root {
        self.root()
    }

    /// Creates a cursor borrowing the single-root multi-level page table.
    ///
    /// This delegates to [`PageTable::cursor`].
    fn cursor(&mut self) -> Self::Cursor<'_> {
        self.cursor()
    }
}

/// The common mutation interface for a single-root cursor.
///
/// Operations preserve the concrete cursor's cleanup behavior.
impl<'a, M, PTE, AL, F> PageTableCursorLike for PageTableCursor<'a, M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// The virtual-address type configured by the metadata.
    ///
    /// It matches the address type of the borrowed multi-level page table.
    type VirtAddr = M::VirtAddr;
    /// The allocator for the borrowed page table hierarchy's table frames.
    ///
    /// Intermediate page table frame allocations use `AL`.
    type Allocator = AL;
    /// The multi-level page table type being mutated by this cursor.
    ///
    /// The cursor borrows the multi-level page table mutably during mutation.
    type PageTable = PageTable<M, PTE, AL, F>;

    /// Maps a virtual range while leaving data frames caller-managed.
    ///
    /// This delegates to [`PageTableCursor::map`].
    fn map(
        &mut self,
        vaddr: Self::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<Self::VirtAddr> {
        self.map(vaddr, paddr, size, flags)
    }

    /// Removes mappings without deallocating their data frames.
    ///
    /// This delegates to [`PageTableCursor::unmap`].
    fn unmap(&mut self, vaddr: Self::VirtAddr, size: usize) -> PagingResult<Self::VirtAddr> {
        self.unmap(vaddr, size)
    }

    /// Processes accumulated invalidations and deallocations.
    ///
    /// Same as [`PageTableCursor::clean_up`].
    fn clean_up(&mut self) {
        self.clean_up();
    }

    /// Transfers accumulated cleanup without processing it.
    ///
    /// # Safety
    ///
    /// The caller must handle the returned cleanup properly after calling this
    /// function, and before any other operations on the cursor.
    ///
    /// See [`PendingCleanup`] for synchronization requirements.
    unsafe fn take_cleanup(&mut self) -> PendingCleanup<Self::VirtAddr> {
        // Safety: the user promised it.
        unsafe { self.take_cleanup() }
    }
}
// }

/// Fallback cleanup when a cursor leaves scope.
///
/// A cursor's destruction processes pending records after partial failure.
impl<'a, M, PTE, AL, F> Drop for PageTableCursor<'a, M, PTE, AL, F>
where
    M: PageTableMeta,
    PTE: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<M::VirtAddr>,
    [(); M::LEVELS]: Sized,
    [(); M::LEVELS - 1]: Sized,
{
    /// Processes cleanup left pending when the cursor leaves scope.
    ///
    /// Same as [`PageTableCursor::clean_up`].
    fn drop(&mut self) {
        self.clean_up();
    }
}
