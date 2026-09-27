//! TLB flush management for page-table mutations.

use core::ops::{Add, AddAssign};

use heapless::Vec as HeaplessVec;

use crate::PageTableMeta;

/// The maximum number of individual page flush requests retained before a full
/// TLB flush.
///
/// Recording one more page than this capacity promotes the pending operation to
/// a full local TLB flush.
pub(crate) const SMALL_FLUSH_THRESHOLD: usize = 32;

/// A TLB invalidation requested by one page-table mutation.
///
/// Requests can leave the TLB unchanged, invalidate one virtual mapping, or
/// invalidate the entire local TLB.
pub enum TlbFlush<M: PageTableMeta> {
    /// No TLB invalidation.
    ///
    /// This is used when the operation did not replace a live leaf mapping.
    None,
    /// Invalidation of the mapping containing one virtual address.
    ///
    /// The address is passed to [`PageTableMeta::flush_tlb`].
    Page(M::VirtAddr),
    /// Invalidation of the entire local TLB.
    ///
    /// This conservative request is used for changes involving huge mappings or
    /// too many individual pages.
    Full,
}

/// TLB invalidations accumulated.
///
/// Individual pages are retained up to [`SMALL_FLUSH_THRESHOLD`], after which
/// the state is promoted to a full flush.
pub enum PendingTlbFlushes<M: PageTableMeta> {
    /// No pending invalidation.
    ///
    /// Flushing this state performs no architecture operation.
    None,
    /// A bounded collection of virtual mappings to invalidate.
    ///
    /// Addresses are flushed individually when the collection is drained.
    Pages(HeaplessVec<M::VirtAddr, SMALL_FLUSH_THRESHOLD>),
    /// A pending invalidation of the entire local TLB.
    ///
    /// Further invalidation requests cannot weaken this state.
    Full,
}

impl<M: PageTableMeta> AddAssign<TlbFlush<M>> for PendingTlbFlushes<M> {
    /// Merges one invalidation request into the pending state.
    ///
    /// Page requests accumulate until the bounded collection is full. A full
    /// request, or an overflowing page collection, permanently promotes the state
    /// to [`PendingTlbFlushes::Full`].
    fn add_assign(&mut self, rhs: TlbFlush<M>) {
        match rhs {
            TlbFlush::None => {}
            TlbFlush::Page(vaddr) => match self {
                PendingTlbFlushes::None => {
                    let mut pages = HeaplessVec::new();
                    // This is safe because the collection is initially empty and the push cannot fail.
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
    /// The pending-flush state produced by the addition.
    ///
    /// Addition consumes and returns the same state type after merging a request.
    type Output = Self;

    /// Merges an invalidation request and returns the updated state.
    ///
    /// This is the consuming counterpart to [`AddAssign::add_assign`].
    fn add(mut self, rhs: TlbFlush<M>) -> Self::Output {
        self += rhs;
        self
    }
}

impl<M: PageTableMeta> PendingTlbFlushes<M> {
    /// Performs and clears all pending TLB invalidations.
    ///
    /// Page requests are issued individually, while a full request invokes the
    /// architecture's full local TLB flush once.
    pub fn flush(&mut self) {
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
