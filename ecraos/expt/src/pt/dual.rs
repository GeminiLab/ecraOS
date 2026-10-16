//! Dual-root multi-level page table implementation.
//!
//! This module provides an architecture-independent Rust abstraction over two independent
//! page table hierarchies. AArch64 uses one hierarchy for the lower address range and
//! a second hierarchy for the upper address range.

use expalloc_trait::PageAllocator;
use memory_addr::PhysAddr;
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::{PagingError, PagingResult},
    meta::{LowerCoverage, PageTableCoverage, PageTableMeta, UpperCoverage, VirtAddr},
    pt::{
        PageTableCursorLike, PageTableLike,
        cleanup::{PendingCleanup, TlbFlusher},
        single::{PageTable, PageTableCursor},
    },
};

/// The physical root addresses of two page table hierarchies.
///
/// Copying these addresses does not change either hierarchy's table-frame lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DualPageTableRoot {
    /// The root used for lower virtual addresses.
    ///
    /// This address alone does not keep the lower page table hierarchy alive.
    pub lower: PhysAddr,
    /// The root used for upper virtual addresses.
    ///
    /// This address alone does not keep the upper page table hierarchy alive.
    pub upper: PhysAddr,
}

/// A dual-root multi-level page table for lower and upper virtual-address ranges.
///
/// The type-level constraints prevent combining symmetric roots or roots with
/// different virtual-address types. The hierarchies may use different geometries
/// and entry types. Both must form valid trees with disjoint table-frame
/// allocations. Both roots use `AL` for their table frames. See the crate-level
/// documentation for table-frame deallocation and hardware-use requirements.
///
/// Each `map` and `unmap` operation is routed to the root selected by its
/// starting virtual address. An individual range must remain within one
/// canonical side and cannot cross the gap between the lower and upper ranges.
pub struct DualPageTable<A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// The lower-address multi-level page table.
    ///
    /// It manages the lower page table hierarchy through the allocator selected by `AL`.
    lower: PageTable<ML, PTEL, AL, F>,
    /// The upper-address multi-level page table.
    ///
    /// It manages the upper page table hierarchy through the allocator selected by `AL`.
    upper: PageTable<MU, PTEU, AL, F>,
}

/// A mutating cursor over lower and upper page table hierarchies.
///
/// The cursor batches cleanup for both roots using `AL` and `F`. Its mutable borrow excludes
/// other Rust mutations through this value. See [`PageTableCursor`] for synchronization
/// requirements.
pub struct DualPageTableCursor<'a, A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// The cursor for the lower-address root.
    ///
    /// Its destruction processes pending cleanup for the lower page table hierarchy.
    lower: PageTableCursor<'a, ML, PTEL, AL, F>,
    /// The cursor for the upper-address root.
    ///
    /// Its destruction processes pending cleanup for the upper page table hierarchy.
    upper: PageTableCursor<'a, MU, PTEU, AL, F>,
}

/// Dual-root construction, storage access, and table-frame lifecycle.
///
/// Both roots use the same static allocator type.
impl<A, ML, PTEL, MU, PTEU, AL, F> DualPageTable<A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// Constructs a dual-root multi-level page table from existing lower and upper hierarchies.
    ///
    /// This function does not inspect either root. The caller must ensure that
    /// both physical addresses identify valid page table roots for their
    /// respective metadata types.
    ///
    /// # Safety
    ///
    /// Each root must satisfy [`PageTable::new_at`]. The two page table hierarchies
    /// must not share table frames.
    pub unsafe fn new_at(root: DualPageTableRoot) -> Self {
        Self {
            lower: unsafe { PageTable::new_at(root.lower) },
            upper: unsafe { PageTable::new_at(root.upper) },
        }
    }

    /// Constructs a dual-root multi-level page table from separate root addresses.
    ///
    /// This is a convenience wrapper around [`Self::new_at`].
    ///
    /// # Safety
    ///
    /// Same as [`Self::new_at`].
    pub unsafe fn new_at_roots(lower: PhysAddr, upper: PhysAddr) -> Self {
        unsafe { Self::new_at(DualPageTableRoot { lower, upper }) }
    }

    /// Allocates and initializes both root page tables through `AL`.
    ///
    /// The roots have the size required by the highest level in `ML` and `MU`
    /// and are zero-initialized before the value is returned.
    /// If the upper-root allocation fails, the lower root is deallocated.
    pub fn new_alloc() -> PagingResult<A, Self>
    where
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        let lower = PageTable::<ML, PTEL, AL, F>::new_alloc()?;
        let upper = PageTable::<MU, PTEU, AL, F>::new_alloc()?;
        Ok(Self { lower, upper })
    }

    /// Consumes the value without deallocating table frames and returns both roots.
    ///
    /// Another dual-root multi-level page table can use the roots if its allocator can
    /// access and deallocate every table frame. Otherwise, the caller must arrange
    /// their deallocation. The allocator type may differ from `AL` if those
    /// requirements hold.
    pub fn leak(self) -> DualPageTableRoot {
        let Self { lower, upper } = self;
        DualPageTableRoot {
            lower: lower.leak(),
            upper: upper.leak(),
        }
    }

    /// Returns both physical root addresses without changing table-frame lifetimes.
    ///
    /// This value must remain alive while its table frames are in use.
    pub const fn root(&self) -> DualPageTableRoot {
        DualPageTableRoot {
            lower: self.lower.root(),
            upper: self.upper.root(),
        }
    }

    /// Creates a cursor for batched mutations and cleanup of both roots.
    ///
    /// See [`DualPageTableCursor`] for synchronization requirements.
    pub fn cursor(&mut self) -> DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU, AL, F> {
        let Self { lower, upper } = self;
        DualPageTableCursor {
            lower: lower.cursor(),
            upper: upper.cursor(),
        }
    }

    /// Returns the lower-address multi-level page table.
    ///
    /// The returned reference borrows the single-root value that manages the lower hierarchy.
    pub const fn lower(&self) -> &PageTable<ML, PTEL, AL, F> {
        &self.lower
    }

    /// Returns the upper-address multi-level page table.
    ///
    /// The returned reference borrows the single-root value that manages the upper hierarchy.
    pub const fn upper(&self) -> &PageTable<MU, PTEU, AL, F> {
        &self.upper
    }

    /// Returns a mutable borrow of the lower-address multi-level page table.
    ///
    /// This borrow does not synchronize hardware page walks or remote TLBs.
    pub fn lower_mut(&mut self) -> &mut PageTable<ML, PTEL, AL, F> {
        &mut self.lower
    }

    /// Returns a mutable borrow of the upper-address multi-level page table.
    ///
    /// This borrow does not synchronize hardware page walks or remote TLBs.
    pub fn upper_mut(&mut self) -> &mut PageTable<MU, PTEU, AL, F> {
        &mut self.upper
    }

    /// Replaces the lower-address multi-level page table and returns the previous value.
    ///
    /// The returned value contains the previous page table hierarchy.
    pub fn swap_lower(&mut self, other: PageTable<ML, PTEL, AL, F>) -> PageTable<ML, PTEL, AL, F> {
        core::mem::replace(&mut self.lower, other)
    }

    /// Replaces the upper-address multi-level page table and returns the previous value.
    ///
    /// The returned value contains the previous page table hierarchy.
    pub fn swap_upper(&mut self, other: PageTable<MU, PTEU, AL, F>) -> PageTable<MU, PTEU, AL, F> {
        core::mem::replace(&mut self.upper, other)
    }
}

/// The common interface for dual-root multi-level page tables.
///
/// Operations delegate to the concrete methods with the same lifecycle contract.
impl<A, ML, PTEL, MU, PTEU, AL, F> PageTableLike for DualPageTable<A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// The virtual-address type shared by both roots.
    ///
    /// Both metadata types require the same semantic address representation.
    type VirtAddr = A;
    /// The pair of physical root addresses.
    ///
    /// Copying these addresses does not change either hierarchy's table-frame lifetime.
    type Root = DualPageTableRoot;
    /// The allocator for both hierarchies' table frames.
    ///
    /// Table access, allocation, and deallocation use `AL`.
    type Allocator = AL;
    /// The TLB flusher for both hierarchies.
    ///
    /// TLB invalidations are performed using `F`.
    type TlbFlusher = F;
    /// The cursor borrowing both multi-level page tables.
    ///
    /// It uses the metadata and entry types of the corresponding roots.
    type Cursor<'a>
        = DualPageTableCursor<'a, A, ML, PTEL, MU, PTEU, AL, F>
    where
        Self: 'a;

    /// Constructs a dual-root multi-level page table from existing lower and upper hierarchies.
    ///
    /// The caller must meet the safety requirements of [`DualPageTable::new_at`].
    unsafe fn new_at(root: Self::Root) -> Self {
        unsafe { Self::new_at(root) }
    }

    /// Allocates and initializes both root page tables.
    ///
    /// This delegates to [`DualPageTable::new_alloc`].
    fn new_alloc() -> PagingResult<Self::VirtAddr, Self> {
        Self::new_alloc()
    }

    /// Consumes the value while retaining both hierarchies' table frames.
    ///
    /// This delegates to [`DualPageTable::leak`] and returns the roots for transfer.
    fn leak(self) -> Self::Root {
        self.leak()
    }

    /// Returns both root addresses without changing table-frame lifetimes.
    ///
    /// This delegates to [`DualPageTable::root`].
    fn root(&self) -> Self::Root {
        self.root()
    }

    /// Creates a cursor borrowing both roots.
    ///
    /// This delegates to [`DualPageTable::cursor`].
    fn cursor(&mut self) -> Self::Cursor<'_> {
        self.cursor()
    }
}

/// The side of dual page table hierarchies that a virtual address belongs to.
///
/// For internal use only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DualPageTableSide {
    /// The lower-address page table hierarchy.
    ///
    /// Operations use the lower root for this side.
    Lower,
    /// The upper-address page table hierarchy.
    ///
    /// Operations use the upper root for this side.
    Upper,
}

/// Selection of a root from a virtual address.
///
/// Canonical coverage determines which root receives each operation.
impl<A, ML, PTEL, MU, PTEU, AL, F> DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// Selects the lower or upper page table hierarchy for a canonical virtual address.
    ///
    /// Addresses accepted by neither root, or by both roots, are rejected.
    fn side(&self, vaddr: A) -> PagingResult<A, DualPageTableSide> {
        let lower =
            <ML::Coverage as PageTableCoverage>::validate_and_truncate_vaddr(vaddr, ML::VA_BITS)
                .is_ok();
        let upper =
            <MU::Coverage as PageTableCoverage>::validate_and_truncate_vaddr(vaddr, MU::VA_BITS)
                .is_ok();

        match (lower, upper) {
            (true, false) => Ok(DualPageTableSide::Lower),
            (false, true) => Ok(DualPageTableSide::Upper),
            _ => Err(PagingError::NonCanonical { vaddr }),
        }
    }

    /// Transfers both roots' pending cleanup into one batch without processing it.
    ///
    /// See [PendingCleanup] for processing and synchronization requirements.
    pub unsafe fn take_cleanup(&mut self) -> PendingCleanup<A> {
        // SAFETY: the user promised it.
        let mut cleanup = unsafe { self.lower.take_cleanup() };
        // SAFETY: they use the same allocator and tlb flusher.
        unsafe { cleanup.append(self.upper.take_cleanup()) };
        cleanup
    }
}

/// The common mutation interface for a dual-root cursor.
///
/// Each mapping operation selects a root from its starting virtual address.
impl<A, ML, PTEL, MU, PTEU, AL, F> PageTableCursorLike
    for DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU, AL, F>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    AL: PageAllocator,
    F: TlbFlusher<A>,
    [(); ML::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    /// The virtual-address type shared by both roots.
    ///
    /// It matches the address type of the borrowed dual-root multi-level page table.
    type VirtAddr = A;
    /// The allocator for both borrowed hierarchies' table frames.
    ///
    /// Intermediate page table frame allocations use `AL`.
    type Allocator = AL;
    /// The multi-level page table type being mutated by this cursor.
    ///
    /// It matches the type of the borrowed dual-root multi-level page table.
    type PageTable = DualPageTable<A, ML, PTEL, MU, PTEU, AL, F>;

    /// Maps a virtual range through the root selected by its starting address.
    ///
    /// Mapped data frames remain caller-managed.
    fn map(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.map(vaddr, paddr, size, flags),
            DualPageTableSide::Upper => self.upper.map(vaddr, paddr, size, flags),
        }
    }

    /// Removes mappings through the root selected by the starting address.
    ///
    /// This does not deallocate the mapped data frames.
    fn unmap(&mut self, vaddr: A, size: usize) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.unmap(vaddr, size),
            DualPageTableSide::Upper => self.upper.unmap(vaddr, size),
        }
    }

    /// Processes both cursors' accumulated cleanup.
    ///
    /// See [`PendingCleanup`] for the selected strategy's synchronization requirements.
    fn clean_up(&mut self) {
        // SAFETY: the user promised it.
        unsafe { self.take_cleanup() }.clean_up();
    }

    /// Transfers both roots' pending cleanup without processing it.
    ///
    /// Same as [DualPageTableCursor::take_cleanup].
    unsafe fn take_cleanup(&mut self) -> PendingCleanup<A> {
        // SAFETY: the user promised it.
        unsafe { self.take_cleanup() }
    }
}
