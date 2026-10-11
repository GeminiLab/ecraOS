//! Common page-table interfaces and concrete implementations.

use expalloc_trait::{DynPageAllocator, PageAllocator};
use memory_addr::PhysAddr;
use page_table_entry::MappingFlags;

use crate::error::PagingResult;
use crate::meta::VirtAddr;

pub mod dual;
pub mod single;

pub use dual::{DualPageTable, DualPageTableCursor, DualPageTableRoot};
pub use single::{PageTable, PageTableCursor};

/// Constrains the common operations of concrete page-table handles.
///
/// This trait primarily provides one behavior contract for [`single::PageTable`]
/// and [`dual::DualPageTable`]. Most callers should use one of those concrete
/// types, or an opaque page-table API when runtime type erasure is required,
/// instead of depending on this trait.
pub trait PageTableLike: Sized {
    /// The semantic virtual-address type accepted by this page table.
    type VirtAddr: VirtAddr;
    /// The value identifying one or more page-table roots.
    type Root: Copy;
    /// The cursor used to mutate this page table.
    type Cursor<'a>: PageTableCursorLike<VirtAddr = Self::VirtAddr>
    where
        Self: 'a;

    /// Creates a page-table handle for an existing root.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the root identifies valid page-table memory
    /// for the concrete implementation.
    unsafe fn new_at(root: Self::Root) -> Self;

    /// Returns the root identifier for this page table.
    fn root(&self) -> Self::Root;

    /// Allocates and initializes a page-table root through a static allocator.
    fn new_alloc<H: PageAllocator>() -> PagingResult<Self::VirtAddr, Self>;

    /// Allocates and initializes a page-table root through a dynamic allocator.
    fn new_alloc_dyn(handler: &DynPageAllocator) -> PagingResult<Self::VirtAddr, Self>;

    /// Creates a cursor for batched page-table mutations.
    fn cursor(&mut self) -> Self::Cursor<'_>;
}

/// Constrains the common mutation operations of page-table cursors.
///
/// This trait primarily provides one behavior contract for
/// [`single::PageTableCursor`] and [`dual::DualPageTableCursor`]. Most callers
/// should use a concrete cursor, or an opaque page-table API when runtime type
/// erasure is required, instead of depending on this trait.
pub trait PageTableCursorLike {
    /// The semantic virtual-address type accepted by this cursor.
    type VirtAddr: VirtAddr;

    /// Maps a virtual address range through a static allocator.
    fn map<H: PageAllocator>(
        &mut self,
        vaddr: Self::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<Self::VirtAddr>;

    /// Maps a virtual address range through a dynamic allocator.
    fn map_dyn(
        &mut self,
        vaddr: Self::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
        handler: &DynPageAllocator,
    ) -> PagingResult<Self::VirtAddr>;

    /// Removes mappings from a virtual address range through a static allocator.
    fn unmap<H: PageAllocator>(
        &mut self,
        vaddr: Self::VirtAddr,
        size: usize,
    ) -> PagingResult<Self::VirtAddr>;

    /// Removes mappings from a virtual address range through a dynamic allocator.
    fn unmap_dyn(
        &mut self,
        vaddr: Self::VirtAddr,
        size: usize,
        handler: &DynPageAllocator,
    ) -> PagingResult<Self::VirtAddr>;

    /// Flushes all TLB invalidations accumulated by this cursor.
    fn flush(&mut self);
}
