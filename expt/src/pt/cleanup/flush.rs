//! TLB flushing strategies and invalidation requests.
//!
//! Cursor operations accumulate requests for the selected [`TlbFlusher`] to process.

use core::ops::{Add, AddAssign};

use dyn_static_traits::dyn_static_traits;
use heapless::Vec as HeaplessVec;

use crate::meta::VirtAddr;

/// A TLB flushing strategy.
///
/// Implementations select the architecture and invalidation scope. A local strategy affects only
/// the calling processor. A strategy covering remote processors must complete the required remote
/// invalidations before returning, including the required translation and page-walk synchronization
/// within that scope.
///
/// Callers must select a strategy suitable for the affected page table hierarchy and its hardware
/// users. An incorrect strategy can leave stale translations or page-walk state when accesses
/// resume. Follow the
/// [safety requirements for page table updates](crate#safely-updating-page-tables).
/// A strategy that waits for remote acknowledgement must not be invoked while the caller holds an
/// IRQ-disabling guard, page-table lock, or allocator lock.
///
/// The same flusher handles every hierarchy using its type. Implementations that need an address
/// space identifier or root must obtain that context outside this interface. Cleanup can request
/// a full flush after topology changes or when individual requests exceed
/// [`SMALL_FLUSH_THRESHOLD`], so both forms must cover the hierarchy's affected translations.
#[dyn_static_traits(DynTlbFlusher)]
pub trait TlbFlusher<A: VirtAddr> {
    /// Invalidates one mapping or all addresses within the strategy's scope.
    ///
    /// `Some(addr)` invalidates the mapping containing `addr`. `None` requests invalidation of all
    /// addresses within this strategy's scope. Required synchronization completes before return.
    fn flush(addr: Option<A>);
}

/// The capacity for individual mapping invalidations before a full flush.
///
/// Recording one more page than this capacity promotes the pending operation to
/// a full TLB flush within the selected strategy's scope.
pub const SMALL_FLUSH_THRESHOLD: usize = 32;

/// A TLB invalidation requested by one page table hierarchy mutation.
///
/// Requests can leave the TLB unchanged, invalidate one virtual mapping, or
/// invalidate all addresses within the selected strategy's scope.
#[derive(Default)]
#[must_use = "the TLB flush must be performed or aggregated into a `TlbFlushes` value"]
pub enum TlbFlush<A: VirtAddr> {
    /// No TLB invalidation.
    ///
    /// This is used when the operation did not replace a live leaf mapping.
    #[default]
    None,
    /// Invalidation of the mapping containing one virtual address.
    ///
    /// The address is passed to the selected [`TlbFlusher`].
    Page(A),
    /// Invalidation of all addresses within the selected strategy's scope.
    ///
    /// This conservative request is used for changes involving huge mappings or newly installed
    /// child page table entries. Accumulating too many page requests also requires a full flush.
    Full,
}

/// An accumulated batch of TLB invalidation requests.
///
/// Individual pages are retained up to [`SMALL_FLUSH_THRESHOLD`], after which
/// the state is promoted to a full flush.
///
/// Dropping or clearing this value performs no invalidation. Call [`Self::flush_with`] to process
/// its requests, or place it in a [`super::PendingCleanup`] batch for automatic cleanup.
/// Repeated addresses are retained and count separately toward the threshold.
#[derive(Default)]
pub enum TlbFlushes<A: VirtAddr> {
    /// No pending invalidation.
    ///
    /// Flushing this state performs no architecture operation.
    #[default]
    None,
    /// A bounded collection of virtual mappings to invalidate.
    ///
    /// Addresses are flushed individually when the collection is drained.
    Pages(HeaplessVec<A, SMALL_FLUSH_THRESHOLD>),
    /// A pending invalidation of all addresses within the selected strategy's scope.
    ///
    /// Further invalidation requests cannot weaken this state.
    Full,
}

/// Accumulates requests and promotes overflowing page lists to full invalidation.
impl<A: VirtAddr> AddAssign<TlbFlush<A>> for TlbFlushes<A> {
    /// Merges one invalidation request into the pending state.
    ///
    /// Page requests accumulate until the bounded collection is full. A full
    /// request, or an overflowing page collection, promotes the state to
    /// [`TlbFlushes::Full`] until the records are consumed.
    fn add_assign(&mut self, rhs: TlbFlush<A>) {
        match rhs {
            TlbFlush::None => {}
            TlbFlush::Page(vaddr) => match self {
                TlbFlushes::None => {
                    let mut pages = HeaplessVec::new();
                    // The collection is initially empty, so this push cannot fail.
                    let _ = pages.push(vaddr);
                    *self = TlbFlushes::Pages(pages);
                }
                TlbFlushes::Pages(pages) => {
                    if pages.push(vaddr).is_err() {
                        *self = TlbFlushes::Full;
                    }
                }
                TlbFlushes::Full => {}
            },
            TlbFlush::Full => *self = TlbFlushes::Full,
        }
    }
}

/// Combines a batch and one request by value.
impl<A: VirtAddr> Add<TlbFlush<A>> for TlbFlushes<A> {
    /// The pending-flush state produced by the addition.
    ///
    /// Addition consumes and returns the same state type after merging a request.
    type Output = Self;

    /// Merges an invalidation request and returns the updated state.
    ///
    /// This is the consuming counterpart to [`AddAssign::add_assign`].
    fn add(mut self, rhs: TlbFlush<A>) -> Self::Output {
        self += rhs;
        self
    }
}

/// Processes or combines recorded invalidations.
impl<A: VirtAddr> TlbFlushes<A> {
    /// Executes the recorded invalidations and clears the list.
    ///
    /// See [`TlbFlusher`] for strategy selection and synchronization requirements.
    pub fn flush_with(&mut self, flusher: &DynTlbFlusher<A>) {
        match self {
            TlbFlushes::None => {}
            TlbFlushes::Pages(pages) => {
                for vaddr in pages.iter().copied() {
                    (flusher.flush)(Some(vaddr));
                }
            }
            TlbFlushes::Full => (flusher.flush)(None),
        }

        self.clear();
    }

    /// Discards every pending request without performing invalidation.
    pub fn clear(&mut self) {
        *self = TlbFlushes::None;
    }

    /// Merges another collection without performing invalidation.
    ///
    /// Full dominates both collections and individual addresses retain their insertion order.
    pub fn append(&mut self, other: Self) {
        match &other {
            Self::None => {}
            Self::Pages(pages) => {
                for &address in pages {
                    *self += TlbFlush::Page(address);
                }
            }
            Self::Full => *self += TlbFlush::Full,
        }
    }
}
