//! Cleanup operations after page table hierarchy mutations.
//!
//! [`PendingCleanup`] performs accumulated TLB invalidations through the selected [`TlbFlusher`].
//! [`TlbFlushes`] stores requests without automatically processing them. Table-frame deallocation
//! happens during mutation and is not part of pending cleanup. See the
//! [safety requirements for page table updates](crate#safely-updating-page-tables).

use core::{mem::ManuallyDrop, ops::AddAssign, ptr};

use crate::meta::VirtAddr;

mod flush;

pub use flush::{DynTlbFlusher, SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlusher, TlbFlushes};

/// Pending TLB invalidations after page table hierarchy mutations.
///
/// Call [`Self::clean_up`] to perform TLB invalidation, or call [`Self::into_inner`] to take the
/// requests and TLB flusher for manual processing.
///
/// Dropping this value also processes cleanup. Explicit cleanup makes the synchronization point
/// visible to the caller.
///
/// The selected [`TlbFlusher`] determines the architecture, invalidation scope, and completion
/// guarantees. See [`TlbFlusher`] for strategy selection and the
/// [safety requirements for page table updates](crate#safely-updating-page-tables).
#[must_use = "call `clean_up` or `into_inner` explicitly, rather than relying on Drop's cleanup"]
pub struct PendingCleanup<A: VirtAddr> {
    /// The pending invalidation requests.
    tlb_flushes: TlbFlushes<A>,
    /// The TLB flusher used to flush the TLB.
    flusher: DynTlbFlusher<A>,
}

/// Constructs, combines, and processes cleanup batches.
impl<A: VirtAddr> PendingCleanup<A> {
    /// Creates an empty [`PendingCleanup`] with the given TLB flusher.
    pub fn new(flusher: &DynTlbFlusher<A>) -> Self {
        Self {
            tlb_flushes: TlbFlushes::None,
            flusher: flusher.clone(),
        }
    }

    /// Creates an empty [`PendingCleanup`] with TLB flusher `F`.
    pub fn new_with<F: TlbFlusher<A>>() -> Self {
        Self::new(&DynTlbFlusher::new::<F>())
    }

    /// Performs the pending TLB invalidations.
    ///
    /// See the type-level contract for invalidation scope and synchronization requirements.
    pub fn clean_up(self) {
        // Just drop it and let the destructor handle it.
        drop(self);
    }

    /// Consumes this batch and returns its requests and TLB flusher without processing them.
    ///
    /// Dropping the returned values does not perform TLB invalidation.
    ///
    /// # Safety
    ///
    /// The caller must process the returned requests through a suitable TLB flusher and complete
    /// any further synchronization required by the
    /// [safety requirements for page table updates](crate#safely-updating-page-tables).
    pub unsafe fn into_inner(self) -> (TlbFlushes<A>, DynTlbFlusher<A>) {
        let this = ManuallyDrop::new(self);

        unsafe {
            (
                ptr::read(&raw const this.tlb_flushes),
                ptr::read(&raw const this.flusher),
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

    /// Transfers another [`PendingCleanup`] into this batch without processing either batch.
    ///
    /// # Safety
    ///
    /// This batch's TLB flusher must provide the invalidation scope and completion guarantees
    /// required by both batches. The other batch's flusher is discarded.
    pub unsafe fn append(&mut self, other: Self) {
        // SAFETY: we take the responsibility for the cleanup.
        let (flushes, _) = unsafe { other.into_inner() };
        self.append_flushes(flushes);
    }
}

/// Processes any requests still held by a cleanup batch.
impl<A: VirtAddr> Drop for PendingCleanup<A> {
    /// Performs the same cleanup as [`Self::clean_up`].
    fn drop(&mut self) {
        self.tlb_flushes.flush_with(&self.flusher);
    }
}
