//! Cleanup operations after page table hierarchy mutations.
//!
//! This module provides types to handle the cleanup operations required after page table hierarchy
//! mutations, namely:
//!
//! - Invalidation of TLB entries.
//! - Deallocation of unused page table frames.
//!
//! [`PendingCleanup`] performs both operations in that order. [`TlbFlushes`] and
//! [`DeallocationList`] store their records without automatically processing them.

use core::{mem::ManuallyDrop, ops::AddAssign, ptr};

use crate::meta::VirtAddr;

mod dealloc;
mod flush;

pub use dealloc::{DeallocationItem, DeallocationList};
use expalloc_trait::{DynPageAllocator, PageAllocator};
pub use flush::{DynTlbFlusher, SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlusher, TlbFlushes};

/// Cleanup obligations accumulated after page table hierarchy mutations.
///
/// Call [`Self::clean_up`] to perform TLB invalidation and then deallocation, or call
/// [`Self::into_parts`] to handle the invalidations and deallocations separately and manually.
///
/// [`Drop`]ping this value has the same effect as calling [`Self::clean_up`], but it's not the
/// recommended way to handle cleanup.
///
/// The selected [`TlbFlusher`] determines the architecture, invalidation scope, and completion
/// guarantees. Deallocation starts after all recorded invalidations return. With a local-only
/// strategy, the caller must complete any required remote synchronization before cleanup, or use
/// [`Self::into_parts`] to coordinate invalidation and deallocation manually. See [`TlbFlusher`]
/// for strategy selection and synchronization requirements.
#[must_use = "call `clean_up` or `into_parts` explicitly, rather than relying on Drop's cleanup"]
pub struct PendingCleanup<A: VirtAddr> {
    /// The invalidations processed before any table-frame deallocation.
    tlb_flushes: TlbFlushes<A>,
    /// Detached table-frame allocations, with their original requested byte sizes.
    deallocations: DeallocationList,
    /// The TLB flusher used to flush the TLB.
    flusher: DynTlbFlusher<A>,
}

impl<A: VirtAddr> PendingCleanup<A> {
    /// Creates an empty [`PendingCleanup`] with the given page allocator and TLB flusher.
    pub fn new(allocator: &DynPageAllocator, flusher: &DynTlbFlusher<A>) -> Self {
        Self {
            tlb_flushes: TlbFlushes::None,
            deallocations: DeallocationList::new(allocator),
            flusher: flusher.clone(),
        }
    }

    /// Creates an empty [`PendingCleanup`] with allocator `AL` and TLB flusher `F`.
    pub fn new_with<AL: PageAllocator, F: TlbFlusher<A>>() -> Self {
        Self::new(&DynPageAllocator::new::<AL>(), &DynTlbFlusher::new::<F>())
    }

    /// Flushes TLB invalidations and then deallocates detached table frames.
    ///
    /// See the type-level contract for invalidation scope and synchronization requirements.
    pub fn clean_up(self) {
        // Just drop it and let the destructor handle it.
        drop(self);
    }

    /// Consumes this batch and returns its records, disabling automatic cleanup.
    ///
    /// The stored TLB flusher is not returned. Neither returned collection processes its records
    /// automatically.
    ///
    /// # Safety
    ///
    /// The caller takes responsibility for the pending cleanup, including using a suitable TLB
    /// flusher and completing required translation and page-walk synchronization before
    /// deallocating recorded allocations. See [`DeallocationList::deallocate_all`] for deallocation
    /// requirements.
    pub unsafe fn into_parts(self) -> (TlbFlushes<A>, DeallocationList) {
        let this = ManuallyDrop::new(self);

        unsafe {
            (
                ptr::read(&raw const this.tlb_flushes),
                ptr::read(&raw const this.deallocations),
            )
        }
    }

    /// Appends another flush batch to this [`PendingCleanup`].
    pub fn append_flushes(&mut self, other: TlbFlushes<A>) {
        self.tlb_flushes.append(other);
    }

    /// Appends a single flush to this [`PendingCleanup`].
    pub fn push_flush(&mut self, flush: TlbFlush<A>) {
        self.tlb_flushes.add_assign(flush);
    }

    /// Appends another deallocation list to this [`PendingCleanup`].
    ///
    /// # Safety
    ///
    /// The combined records must satisfy [`DeallocationList::deallocate_all_with`]'s safety
    /// requirements when this batch deallocates them through its allocator after invalidation.
    /// Transferring list storage must also satisfy [`DeallocationList::append`]'s requirements.
    pub unsafe fn append_deallocations(&mut self, other: DeallocationList) {
        // SAFETY: the user promised it.
        unsafe { self.deallocations.append(other) }
    }

    /// Appends a new deallocation item to this [`PendingCleanup`].
    ///
    /// # Safety
    ///
    /// Adding this item must satisfy the record requirements of [`Self::append_deallocations`].
    /// If storage cannot grow, the item is returned and its deallocation remains the caller's
    /// responsibility.
    pub unsafe fn push_deallocation(
        &mut self,
        item: DeallocationItem,
    ) -> Result<(), DeallocationItem> {
        self.deallocations.push(item)
    }

    /// Transfers another [`PendingCleanup`] into this batch without processing either batch.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that:
    ///
    /// - This batch's TLB flusher provides the invalidation scope and completion guarantees
    ///   required by both batches.
    /// - Transferring the other batch's records and list storage satisfies
    ///   [`Self::append_deallocations`]'s safety requirements.
    pub unsafe fn append(&mut self, other: Self) {
        // SAFETY: we take the responsibility for the cleanup.
        let (flushes, deallocations) = unsafe { other.into_parts() };
        self.append_flushes(flushes);
        // SAFETY: the user promised it.
        unsafe { self.append_deallocations(deallocations) };
    }
}

impl<A: VirtAddr> Drop for PendingCleanup<A> {
    /// Performs the same cleanup as [`Self::clean_up`].
    fn drop(&mut self) {
        self.tlb_flushes.flush_with(&self.flusher);
        // SAFETY: the user promised it.
        unsafe { self.deallocations.deallocate_all() };
    }
}
