//! Dual-root page tables.
//!
//! This module contains the architecture-independent representation of a pair
//! of page-table roots. AArch64 uses one root for the lower address range and a
//! second root for the upper address range.

use expalloc_trait::{DynPageAllocator, PageAllocator};
use maybe_non_generic::maybe_non_generic;
use memory_addr::PhysAddr;
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::{PagingError, PagingResult},
    meta::{LowerCoverage, PageTableCoverage, PageTableMeta, UpperCoverage, VirtAddr},
    pt::{
        PageTableCursorLike, PageTableLike,
        single::{PageTable, PageTableCursor},
    },
};

/// The physical roots of a dual page table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DualPageTableRoot {
    /// The root used for lower virtual addresses.
    pub lower: PhysAddr,
    /// The root used for upper virtual addresses.
    pub upper: PhysAddr,
}

/// A pair of page tables covering the lower and upper virtual-address ranges.
///
/// The type-level constraints make it impossible to construct a dual table from
/// two symmetric roots or from roots using different virtual-address types. The
/// two roots may still use different page-table geometries and entry types.
///
/// Each `map` and `unmap` operation is routed to the root selected by its
/// starting virtual address. An individual range must remain within one
/// canonical side and cannot cross the gap between the lower and upper ranges.
pub struct DualPageTable<A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
{
    /// The lower-address translation root.
    lower: PageTable<ML, PTEL>,
    /// The upper-address translation root.
    upper: PageTable<MU, PTEU>,
}

/// A mutating cursor over a dual page table.
pub struct DualPageTableCursor<'a, A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
{
    /// The cursor for the lower-address root.
    lower: PageTableCursor<'a, ML, PTEL>,
    /// The cursor for the upper-address root.
    upper: PageTableCursor<'a, MU, PTEU>,
}

impl<A, ML, PTEL, MU, PTEU> DualPageTable<A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
{
    /// Creates a dual page-table handle for existing roots.
    ///
    /// This function does not inspect either root. The caller must ensure that
    /// both physical addresses identify valid page-table roots for their
    /// respective metadata types.
    ///
    /// # Safety
    ///
    /// The caller must ensure that both roots in `root` are valid physical
    /// addresses for the selected page-table formats.
    pub unsafe fn new_at(root: DualPageTableRoot) -> Self {
        Self {
            lower: unsafe { PageTable::new_at(root.lower) },
            upper: unsafe { PageTable::new_at(root.upper) },
        }
    }

    /// Creates a dual page-table handle from two existing roots.
    ///
    /// This is a convenience wrapper around [`Self::new_at`].
    ///
    /// # Safety
    ///
    /// The caller must ensure that both physical addresses identify valid roots
    /// for their respective metadata types.
    pub unsafe fn new_at_roots(lower: PhysAddr, upper: PhysAddr) -> Self {
        unsafe { Self::new_at(DualPageTableRoot { lower, upper }) }
    }

    /// Allocates and initializes both page-table roots through the specified
    /// page allocator.
    ///
    /// The roots have the size required by the highest level in `ML` and `MU`
    /// and are zero-initialized before the handle is returned.
    #[maybe_non_generic(
        new_alloc_dyn,
        type(H => handler: &DynPageAllocator),
        fn(PageTable::new_alloc => PageTable::new_alloc_dyn)
    )]
    pub fn new_alloc<H: PageAllocator>() -> PagingResult<A, Self>
    where
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        let lower = PageTable::<ML, PTEL>::new_alloc::<H>()?;
        let upper = PageTable::<MU, PTEU>::new_alloc::<H>()?;
        Ok(Self { lower, upper })
    }

    /// Returns both physical roots of the dual page table.
    pub const fn root(&self) -> DualPageTableRoot {
        DualPageTableRoot {
            lower: self.lower.root(),
            upper: self.upper.root(),
        }
    }

    /// Creates a cursor for batched mutations of both roots.
    pub fn cursor(&mut self) -> DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU> {
        let Self { lower, upper } = self;
        DualPageTableCursor {
            lower: lower.cursor(),
            upper: upper.cursor(),
        }
    }

    /// Returns the lower-address page-table root.
    pub const fn lower(&self) -> &PageTable<ML, PTEL> {
        &self.lower
    }

    /// Returns the upper-address page-table root.
    pub const fn upper(&self) -> &PageTable<MU, PTEU> {
        &self.upper
    }

    /// Returns the mutable lower-address page-table root.
    pub fn lower_mut(&mut self) -> &mut PageTable<ML, PTEL> {
        &mut self.lower
    }

    /// Returns the mutable upper-address page-table root.
    pub fn upper_mut(&mut self) -> &mut PageTable<MU, PTEU> {
        &mut self.upper
    }

    /// Swaps the lower-address page-table root with the given one, returning the old root.
    pub fn swap_lower(&mut self, other: PageTable<ML, PTEL>) -> PageTable<ML, PTEL> {
        core::mem::replace(&mut self.lower, other)
    }

    /// Swaps the upper-address page-table root with the given one, returning the old root.
    pub fn swap_upper(&mut self, other: PageTable<MU, PTEU>) -> PageTable<MU, PTEU> {
        core::mem::replace(&mut self.upper, other)
    }
}

impl<A, ML, PTEL, MU, PTEU> PageTableLike for DualPageTable<A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
    [(); ML::LEVELS - 1]: Sized,
    [(); MU::LEVELS - 1]: Sized,
{
    type VirtAddr = A;
    type Root = DualPageTableRoot;
    type Cursor<'a>
        = DualPageTableCursor<'a, A, ML, PTEL, MU, PTEU>
    where
        Self: 'a;

    unsafe fn new_at(root: Self::Root) -> Self {
        unsafe { Self::new_at(root) }
    }

    fn root(&self) -> Self::Root {
        self.root()
    }

    fn new_alloc<H: PageAllocator>() -> PagingResult<Self::VirtAddr, Self> {
        Self::new_alloc::<H>()
    }

    fn new_alloc_dyn(handler: &DynPageAllocator) -> PagingResult<Self::VirtAddr, Self> {
        Self::new_alloc_dyn(handler)
    }

    fn cursor(&mut self) -> Self::Cursor<'_> {
        self.cursor()
    }
}

/// The side of a dual page table that a virtual address belongs to.
///
/// For internal use only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DualPageTableSide {
    /// The lower-address side of the dual page table.
    Lower,
    /// The upper-address side of the dual page table.
    Upper,
}

impl<A, ML, PTEL, MU, PTEU> DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
{
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
}

impl<A, ML, PTEL, MU, PTEU> PageTableCursorLike for DualPageTableCursor<'_, A, ML, PTEL, MU, PTEU>
where
    A: VirtAddr,
    ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
    MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
    PTEL: GenericPTE,
    PTEU: GenericPTE,
    [(); ML::LEVELS]: Sized,
    [(); MU::LEVELS]: Sized,
{
    type VirtAddr = A;

    fn map<H: PageAllocator>(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.map::<H>(vaddr, paddr, size, flags),
            DualPageTableSide::Upper => self.upper.map::<H>(vaddr, paddr, size, flags),
        }
    }

    fn map_dyn(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
        handler: &DynPageAllocator,
    ) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.map_dyn(vaddr, paddr, size, flags, handler),
            DualPageTableSide::Upper => self.upper.map_dyn(vaddr, paddr, size, flags, handler),
        }
    }

    fn unmap<H: PageAllocator>(&mut self, vaddr: A, size: usize) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.unmap::<H>(vaddr, size),
            DualPageTableSide::Upper => self.upper.unmap::<H>(vaddr, size),
        }
    }

    fn unmap_dyn(&mut self, vaddr: A, size: usize, handler: &DynPageAllocator) -> PagingResult<A> {
        match self.side(vaddr)? {
            DualPageTableSide::Lower => self.lower.unmap_dyn(vaddr, size, handler),
            DualPageTableSide::Upper => self.upper.unmap_dyn(vaddr, size, handler),
        }
    }

    fn flush(&mut self) {
        self.lower.flush();
        self.upper.flush();
    }
}
