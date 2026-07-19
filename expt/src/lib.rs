#![no_std]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(generic_const_items)]

#[cfg(test)]
extern crate std;

pub mod arch;
mod meta;
pub mod opaque;
pub mod pte {
    pub use page_table_entry::*;
}

use core::{
    marker::PhantomData,
    ops::{Add, AddAssign},
};

use expalloc_trait::{DynPageAllocator, PageAllocator};
use heapless::Vec as HeaplessVec;
use maybe_non_generic::maybe_non_generic;
use memory_addr::{AddrRange, MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

pub use meta::PageTableMeta;

#[derive(Debug, thiserror::Error)]
pub enum PagingError {
    #[error("The page is not mapped")]
    NotMapped,
    #[error("The page is already mapped")]
    AlreadyMapped,
    #[error("The page is mapped to a huge page")]
    MappedToHugePage,
    #[error("Allocation failed")]
    AllocationFailed,
    #[error("The page cannot be a page at level {level}")]
    CannotBePage { level: usize },
}

pub type PagingResult<T = ()> = Result<T, PagingError>;

// #[dyn_static_traits(DynPageAllocator)]
// pub trait PageAllocator {
//     fn alloc_page_aligned(bytes_required: usize) -> Option<PhysAddr>;
//     fn dealloc_page_aligned(addr: PhysAddr, bytes_deallocated: usize);
//     fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
// }

struct PageTableMetaAssertions<M: PageTableMeta> {
    _phantom: PhantomData<M>,
}

impl<M: PageTableMeta> PageTableMetaAssertions<M> {
    const VA_BITS_ASSERTIONS: () = assert!(M::VA_BITS <= 64, "Virtual address space must be no more than 64 bits") where [(); M::LEVELS]: Sized;
    const LEVELS_ASSERTIONS: () = assert!(
        M::LEVELS <= 5 && M::LEVELS > 0,
        "Page table level must be no more than 5 and greater than 0"
    );
    const MAX_PAGE_LEVEL_ASSERTIONS: () = assert!(
        // 0 <= M::MAX_PAGE_LEVEL && 
        M::MAX_PAGE_LEVEL < M::LEVELS,
        "`M::MAX_PAGE_LEVEL` must be greater or equal than 0 and less than `M::LEVELS`"
    ) where [(); M::LEVELS]: Sized;

    #[doc(hidden)]
    #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
    pub const ASSERTIONS: () = {
        let _ = Self::VA_BITS_ASSERTIONS;
        let _ = Self::LEVELS_ASSERTIONS;
        let _ = Self::MAX_PAGE_LEVEL_ASSERTIONS;
    } where [(); M::LEVELS]: Sized;
}

const SMALL_FLUSH_THRESHOLD: usize = 32;

enum TlbFlush<M: PageTableMeta> {
    None,
    Page(M::VirtAddr),
    Full,
}

enum PendingTlbFlushes<M: PageTableMeta> {
    None,
    Pages(HeaplessVec<M::VirtAddr, SMALL_FLUSH_THRESHOLD>),
    Full,
}

impl<M: PageTableMeta> AddAssign<TlbFlush<M>> for PendingTlbFlushes<M> {
    fn add_assign(&mut self, rhs: TlbFlush<M>) {
        match rhs {
            TlbFlush::None => {}
            TlbFlush::Page(vaddr) => match self {
                PendingTlbFlushes::None => {
                    let mut pages = HeaplessVec::new();
                    let _ = pages.push(vaddr);
                    *self = PendingTlbFlushes::Pages(pages);
                }
                PendingTlbFlushes::Pages(pages) => {
                    if pages.push(vaddr).is_err() {
                        *self = PendingTlbFlushes::Full;
                    }
                }
                PendingTlbFlushes::Full => {}
            },
            TlbFlush::Full => *self = PendingTlbFlushes::Full,
        }
    }
}

impl<M: PageTableMeta> Add<TlbFlush<M>> for PendingTlbFlushes<M> {
    type Output = Self;

    fn add(mut self, rhs: TlbFlush<M>) -> Self::Output {
        self += rhs;
        self
    }
}

impl<M: PageTableMeta> PendingTlbFlushes<M> {
    fn flush(&mut self) {
        match self {
            PendingTlbFlushes::None => {}
            PendingTlbFlushes::Pages(pages) => {
                for vaddr in pages.iter().copied() {
                    M::flush_tlb(Some(vaddr));
                }
            }
            PendingTlbFlushes::Full => M::flush_tlb(None),
        }
        *self = PendingTlbFlushes::None;
    }
}

/// A mutating cursor over a page table.
///
/// Cursor operations record the virtual mappings they affect and flush the
/// local TLB when [`Self::flush`] is called or when the cursor is dropped.
pub struct PageTableCursor<'a, M: PageTableMeta, PTE: GenericPTE>
where
    [(); M::LEVELS]: Sized,
{
    table: &'a mut PageTable<M, PTE>,
    pending_flushes: PendingTlbFlushes<M>,
}

pub struct PageTable<M: PageTableMeta, PTE: GenericPTE> {
    root: PhysAddr,
    _phantom: PhantomData<(PTE, M)>,
}

fn checked_page_end<A: MemoryAddr>(start: A, end: A, page_size: usize) -> Option<A> {
    let next = usize::checked_add(start.into(), page_size)?;
    if next <= end.into() {
        Some(A::from(next))
    } else {
        None
    }
}

impl<M: PageTableMeta, PTE: GenericPTE> PageTable<M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// Creates a new page table at the given physical address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the physical address is valid.
    pub unsafe fn new_at(paddr: PhysAddr) -> Self {
        #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
        let _ = PageTableMetaAssertions::<M>::ASSERTIONS;

        Self {
            root: paddr,
            _phantom: PhantomData,
        }
    }

    /// Allocates and initializes a new root page table through `H`.
    #[maybe_non_generic(
        new_alloc_dyn,
        type(H => handler: DynPageAllocator),
        fn(Self::alloc_table => Self::alloc_table_dyn)
    )]
    pub fn new_alloc<H: PageAllocator>() -> PagingResult<Self>
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

    const fn table_size<const LEVEL: usize>() -> usize {
        size_of::<PTE>() << M::LEVEL_BITS[LEVEL]
    }

    const fn index<const LEVEL: usize>(vaddr: usize) -> usize {
        let (start, end) = M::LEVEL_BIT_RANGES[LEVEL];

        (vaddr >> start) & ((1 << (end - start)) - 1)
    }

    /// Gets the table at level `LEVEL` from its physical address `paddr`.
    #[maybe_non_generic(table_of_mut_non_const, const(LEVEL => level: usize))]
    #[maybe_non_generic(table_of_mut_dyn, type(H => handler: DynPageAllocator))]
    #[maybe_non_generic(
        table_of_mut_non_const_dyn,
        const(LEVEL => level: usize),
        type(H => handler: DynPageAllocator),
    )]
    fn table_of_mut<'a, const LEVEL: usize, H: PageAllocator>(paddr: PhysAddr) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];

        unsafe {
            let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }

    #[maybe_non_generic(alloc_table_dyn, type(H => handler: DynPageAllocator))]
    fn alloc_table<const LEVEL: usize, H: PageAllocator>() -> PagingResult<PhysAddr> {
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

    #[maybe_non_generic(
        get_page_entry_mut_dyn,
        type(H => handler: DynPageAllocator),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(self.next_table_mut => self.next_table_mut_dyn),
        fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn),
    )]
    fn get_page_entry_mut<H: PageAllocator>(
        &mut self,
        vaddr: M::VirtAddr,
        level: usize,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<(&mut PTE, usize)> {
        let vaddr_usize: usize = vaddr.into();

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
                        let index4 = PageTable::<M, PTE>::index::<4>(vaddr_usize);
                        let p4e = &mut p4[index4];

                        if level == 4 {
                            self.clear_pte::<H>(p4e, 4, vaddr)?;
                            return Ok((p4e, index4));
                        }

                        self.next_table_mut::<3, H>(p4e, create_if_not_exists, split_huge_page)?
                    } else {
                        PageTable::<M, PTE>::table_of_mut::<3, H>(self.table.root)
                    };
                    let index3 = PageTable::<M, PTE>::index::<3>(vaddr_usize);
                    let p3e = &mut p3[index3];

                    if level == 3 {
                        self.clear_pte::<H>(p3e, 3, vaddr)?;
                        return Ok((p3e, index3));
                    }

                    self.next_table_mut::<2, H>(p3e, create_if_not_exists, split_huge_page)?
                } else {
                    PageTable::<M, PTE>::table_of_mut::<2, H>(self.table.root)
                };
                let index2 = PageTable::<M, PTE>::index::<2>(vaddr_usize);
                let p2e = &mut p2[index2];

                if level == 2 {
                    self.clear_pte::<H>(p2e, 2, vaddr)?;
                    return Ok((p2e, index2));
                }

                self.next_table_mut::<1, H>(p2e, create_if_not_exists, split_huge_page)?
            } else {
                PageTable::<M, PTE>::table_of_mut::<1, H>(self.table.root)
            };
            let index1 = PageTable::<M, PTE>::index::<1>(vaddr_usize);
            let p1e = &mut p1[index1];

            if level == 1 {
                self.clear_pte::<H>(p1e, 1, vaddr)?;
                return Ok((p1e, index1));
            }

            self.next_table_mut::<0, H>(p1e, create_if_not_exists, split_huge_page)?
        } else {
            PageTable::<M, PTE>::table_of_mut::<0, H>(self.table.root)
        };
        let index0 = PageTable::<M, PTE>::index::<0>(vaddr_usize);
        let p0e = &mut p0[index0];

        self.clear_pte::<H>(p0e, 0, vaddr)?;
        Ok((p0e, index0))
    }

    /// Gets the table at level `LEVEL` from a PTE of the level above.
    #[maybe_non_generic(
        next_table_mut_dyn,
        type(H => handler: DynPageAllocator),
        fn(PageTable::alloc_table => PageTable::alloc_table_dyn),
        fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn),
    )]
    fn next_table_mut<'a, const LEVEL: usize, H: PageAllocator>(
        &mut self,
        entry: &mut PTE,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<&'a mut [PTE]> {
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

    #[maybe_non_generic(
        clear_pte_dyn,
        type(H => handler: DynPageAllocator),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(PageTable::table_of_mut_non_const => PageTable::table_of_mut_non_const_dyn),
    )]
    fn clear_pte<H: PageAllocator>(
        &mut self,
        entry: &mut PTE,
        level: usize,
        vaddr: M::VirtAddr,
    ) -> PagingResult {
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

    /// Split the given range into pages (as large and less as possible), remove
    /// all current mappings on the range, and call the given function for PTEs
    /// of each page.
    ///
    /// The parameters of the function are:
    /// - The level of the PTE: `0` for the lowest level, `M::LEVELS - 1` for
    ///   the highest level,
    /// - The index of the PTE in the table of the level,
    /// - The start virtual address of the page, and
    /// - The mutable reference to the PTE of the page.
    ///
    /// Before calling the function, the PTE is [cleared](GenericPTE::clear). If
    /// the PTE points to a table, descendant entries are cleared recursively and
    /// table pages are retained. If the range overlaps with a huge page, the
    /// page is split into smaller pages.
    ///
    /// The range is aligned to the page size of the lowest level.
    #[maybe_non_generic(
        iter_pages_in_range_dyn,
        type(H => handler: DynPageAllocator),
        fn(self.get_page_entry_mut => self.get_page_entry_mut_dyn),
    )]
    fn iter_pages_in_range<F, H: PageAllocator>(
        &mut self,
        range: AddrRange<M::VirtAddr>,
        mut f: F,
    ) -> PagingResult
    where
        F: FnMut(usize, usize, M::VirtAddr, &mut PTE) -> PagingResult<TlbFlush<M>>,
    {
        let mut start_vaddr = range.start.align_down(M::LEVEL_PAGE_SIZE[0]);
        let end_vaddr = range.end.align_up(M::LEVEL_PAGE_SIZE[0]);

        while start_vaddr < end_vaddr {
            for level in (0..=M::MAX_PAGE_LEVEL).rev() {
                let page_size = M::LEVEL_PAGE_SIZE[level];
                if start_vaddr.is_aligned(page_size) {
                    let Some(next_vaddr) = checked_page_end(start_vaddr, end_vaddr, page_size)
                    else {
                        continue;
                    };

                    let flush = {
                        let (entry, index) =
                            self.get_page_entry_mut::<H>(start_vaddr, level, true, true)?;
                        f(level, index, start_vaddr, entry)?
                    };
                    self.pending_flushes += flush;
                    start_vaddr = next_vaddr;
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
        type(H => handler: DynPageAllocator),
        fn(self.iter_pages_in_range => self.iter_pages_in_range_dyn),
    )]
    pub fn map<H: PageAllocator>(
        &mut self,
        vaddr: M::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
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
        type(H => handler: DynPageAllocator),
        fn(self.iter_pages_in_range => self.iter_pages_in_range_dyn),
    )]
    pub fn unmap<H: PageAllocator>(&mut self, vaddr: M::VirtAddr, size: usize) -> PagingResult {
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
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests;
