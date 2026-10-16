//! Common multi-level page table interfaces and concrete implementations.
//!
//! The traits describe construction, table-frame management, and cursor-based mutation for
//! single-root and dual-root multi-level page tables.

use expalloc_trait::PageAllocator;
use memory_addr::PhysAddr;
use page_table_entry::MappingFlags;

use crate::{error::PagingResult, meta::VirtAddr, pt::cleanup::TlbFlusher};

pub mod cleanup;
pub mod dual;
pub mod single;

use cleanup::PendingCleanup;
pub use dual::{DualPageTable, DualPageTableCursor, DualPageTableRoot};
pub use single::{PageTable, PageTableCursor};

/// The common interface for concrete multi-level page table types.
///
/// This trait is primarily used to constrain the behavior of [`single::PageTable`]
/// and [`dual::DualPageTable`]. Most callers should use one of those concrete types,
/// or [`crate::opaque::OpaquePageTable`] when runtime type erasure is required.
pub trait PageTableLike: Sized {
    /// The semantic virtual-address type accepted by this multi-level page table.
    ///
    /// The type determines the addresses accepted by mapping operations.
    type VirtAddr: VirtAddr;
    /// The value identifying one or more page table roots.
    ///
    /// Copying this value does not change the table frames' lifetime. Therefore,
    /// the user must not use this value after dropping the page table hierarchy.
    type Root: Copy;
    /// The allocator for table frames.
    ///
    /// Allocations, physical-address translation, and deallocation use this type.
    type Allocator: PageAllocator;
    /// The TLB flusher for this page table hierarchy.
    ///
    /// TLB invalidations are performed using this type.
    type TlbFlusher: TlbFlusher<Self::VirtAddr>;
    /// The cursor used to mutate this page table hierarchy.
    ///
    /// The cursor borrows the multi-level page table mutably during mutation.
    type Cursor<'a>: PageTableCursorLike<
            VirtAddr = Self::VirtAddr,
            Allocator = Self::Allocator,
            PageTable = Self,
        >
    where
        Self: 'a;

    /// Constructs a multi-level page table from an existing page table hierarchy.
    ///
    /// # Safety
    ///
    /// The root must satisfy the requirements of [`PageTable::new_at`] for a single-root
    /// implementation, or [`DualPageTable::new_at`] for a dual-root implementation.
    unsafe fn new_at(root: Self::Root) -> Self;

    /// Allocates and initializes a multi-level page table through the selected allocator.
    ///
    /// Only the root page tables are allocated initially. Child page tables are allocated
    /// as needed by cursor operations.
    fn new_alloc() -> PagingResult<Self::VirtAddr, Self>;

    /// Returns the root identifier without changing the table frames' lifetime.
    ///
    /// The returned physical address (or pair of physical addresses for dual-root
    /// multi-level page tables) must not be used after dropping this value.
    fn root(&self) -> Self::Root;

    /// Consumes the multi-level page table without deallocating its table frames.
    ///
    /// Another multi-level page table can use the returned root if its allocator can
    /// access and deallocate every existing table frame. Otherwise, the caller must
    /// arrange their deallocation.
    fn leak(self) -> Self::Root;

    /// Creates a cursor for batched mutations.
    ///
    /// See [PageTableCursorLike] for batching and cleanup behavior.
    fn cursor(&mut self) -> Self::Cursor<'_>;
}

/// The common interface for cursors over page table hierarchies.
///
/// This trait is the common interface for [`single::PageTableCursor`] and
/// [`dual::DualPageTableCursor`]. A cursor's mutable borrow excludes other Rust
/// mutations through the borrowed multi-level page table but does not synchronize
/// hardware page walks.
///
/// Map and unmap round ranges outward to base-page boundaries. A successful return means the PTE
/// mutations are complete, with cleanup still pending. Errors retain earlier mutations, including
/// any intermediate tables already installed. Unattached allocations from failed preparation are
/// deallocated. Operations are not transactional.
///
/// Cleanup remains pending until [`Self::clean_up`] processes it or [`Self::take_cleanup`]
/// transfers it to the caller. Implementations also process any remaining cleanup automatically
/// when the cursor leaves scope.
pub trait PageTableCursorLike {
    /// The semantic virtual-address type accepted by this cursor.
    ///
    /// It matches the virtual-address type of the borrowed multi-level page table.
    type VirtAddr: VirtAddr;
    /// The allocator used by this cursor.
    ///
    /// Intermediate page tables are allocated through the borrowed multi-level page table's
    /// allocator.
    type Allocator: PageAllocator;
    /// The multi-level page table type being mutated by this cursor.
    ///
    /// The cursor borrows the multi-level page table mutably during mutation.
    type PageTable: PageTableLike;

    /// Maps a virtual range to physical memory.
    ///
    /// This operation changes page table entries while mapped data frames remain
    /// caller-managed. Replacing a child-table descriptor records its table-frame allocations for
    /// deferred deallocation. See the trait-level contract for partial failure and cleanup.
    fn map(
        &mut self,
        vaddr: Self::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<Self::VirtAddr>;

    /// Removes mappings from a virtual address range.
    ///
    /// This operation clears mappings but does not deallocate their data frames.
    /// Intermediate tables remain allocated. See the trait-level contract for partial failure
    /// and cleanup.
    fn unmap(&mut self, vaddr: Self::VirtAddr, size: usize) -> PagingResult<Self::VirtAddr>;

    /// Processes pending cleanup using the selected TLB flushing strategy and allocator.
    ///
    /// See [`PendingCleanup`] for synchronization requirements.
    fn clean_up(&mut self);

    /// Transfers all pending cleanup out of the cursor without processing it.
    ///
    /// # Safety
    ///
    /// The caller must handle the returned cleanup properly after calling this
    /// function, and before any other operations on the cursor.
    ///
    /// See [`PendingCleanup`] for synchronization requirements.
    unsafe fn take_cleanup(&mut self) -> PendingCleanup<Self::VirtAddr>;
}
