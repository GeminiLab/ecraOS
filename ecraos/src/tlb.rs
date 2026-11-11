//! Semantic cross-CPU translation invalidation.
//!
//! Mapping transactions reuse the common acknowledged IPI protocol.

use alloc::vec::Vec;
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use exarch::mem::LocalTlbFlusher;
use expt::pt::cleanup::{DynTlbFlusher, SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlushes};
use memory_addr::{MemoryAddr, VirtAddr, va};

use crate::{
    ipi,
    mem::vmm,
    mp::{self, LogicalCpuId},
};

/// A rejected request or incomplete remote invalidation.
///
/// Common IPI errors preserve the target, generation and absolute deadline diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlbShootdownError {
    /// An invalid page size or unaligned virtual address.
    ///
    /// Validation precedes publication and delivery.
    InvalidRequest,
    /// An accidentally included local CPU.
    ///
    /// The caller has already performed local invalidation.
    LocalTarget,
    /// A common exact-generation completion failure.
    ///
    /// Resources must remain quarantined after a PTE mutation.
    Ipi(ipi::IpiError),
}

/// Validates every requested page against the active page granule.
///
/// Empty and full batches need no addresses. Page lists must contain only aligned addresses.
pub(crate) fn validate(
    requests: &TlbFlushes<VirtAddr>,
    size: usize,
) -> Result<(), TlbShootdownError> {
    let valid = size.is_power_of_two()
        && match requests {
            TlbFlushes::Pages(pages) => pages.iter().all(|address| address.is_aligned(size)),
            TlbFlushes::None | TlbFlushes::Full => true,
        };
    valid.then_some(()).ok_or(TlbShootdownError::InvalidRequest)
}

/// Removes the caller from the MP-owned ready snapshot.
///
/// MP alone filters Online and IPI-ready lifecycle conditions.
pub(crate) fn remote_targets(
    mut targets: Vec<LogicalCpuId>,
    current: LogicalCpuId,
) -> Vec<LogicalCpuId> {
    targets.retain(|&target| target != current);
    targets
}

/// Atomic storage for the single serialized semantic request.
///
/// All fields use sequential consistency so readers bracketing a payload with generation loads
/// cannot accept mixed fields while a timed-out generation is replaced. This includes the
/// Release/Acquire publication guarantee without a lock or non-atomic seqlock data race.
struct Payload {
    /// The published generation, or zero while being replaced.
    ///
    /// Common IPI generations are never zero or reused.
    generation: AtomicU64,
    /// The number of pages, zero for an empty batch, or `usize::MAX` for full invalidation.
    count: AtomicUsize,
    /// Virtual addresses stored atomically for readers of a potentially timed-out generation.
    pages: [AtomicUsize; SMALL_FLUSH_THRESHOLD],
}

/// Publishes and decodes TLB requests for one IPI generation.
impl Payload {
    /// Creates unpublished atomic request storage.
    ///
    /// No generation can decode until the first complete publication.
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            count: AtomicUsize::new(0),
            pages: [const { AtomicUsize::new(0) }; SMALL_FLUSH_THRESHOLD],
        }
    }

    /// Publishes all fields before the common pending reasons become visible.
    ///
    /// The VMM's Transitioning phase gives this storage exactly one writer.
    fn publish(&self, generation: u64, requests: &TlbFlushes<VirtAddr>) {
        self.generation.store(0, Ordering::SeqCst);
        let count = match requests {
            TlbFlushes::None => 0,
            TlbFlushes::Pages(pages) => {
                for (slot, address) in self.pages.iter().zip(pages) {
                    slot.store(address.as_usize(), Ordering::SeqCst);
                }
                pages.len()
            }
            TlbFlushes::Full => usize::MAX,
        };
        self.count.store(count, Ordering::SeqCst);
        self.generation.store(generation, Ordering::SeqCst);
    }

    /// Decodes only an unchanged exact-generation publication.
    ///
    /// A delayed interrupt cannot acknowledge a newer payload on behalf of an older request.
    fn decode(&self, generation: u64) -> Option<TlbFlushes<VirtAddr>> {
        if generation == 0 || self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        let count = self.count.load(Ordering::SeqCst);
        let mut requests = TlbFlushes::None;
        if count == usize::MAX {
            requests = TlbFlushes::Full;
        } else if count <= SMALL_FLUSH_THRESHOLD {
            for slot in &self.pages[..count] {
                requests += TlbFlush::Page(va!(slot.load(Ordering::SeqCst)));
            }
        } else {
            return None;
        }
        if self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        Some(requests)
    }
}

/// The one request shared by serialized kernel mapping transactions.
///
/// This stores only payload, leaving delivery, target inventory and acknowledgement with IPI/MP.
static REQUEST: Payload = Payload::new();

/// Returns the generation published by the current serialized transaction.
///
/// Foundation callers retain the transaction guard so a newer request cannot replace this value.
#[cfg(feature = "foundation-test")]
pub(crate) fn published_generation() -> u64 {
    REQUEST.generation.load(Ordering::SeqCst)
}

/// Returns the published requests if their generation still matches.
#[cfg(feature = "foundation-test")]
pub(crate) fn published_requests(generation: u64) -> Option<TlbFlushes<VirtAddr>> {
    REQUEST.decode(generation)
}

/// Installs the semantic handler before any CPU becomes IPI-ready.
///
/// Registration is immutable throughout the Online CPU lifetime.
pub(crate) fn init() {
    ipi::register_tlb_handler(handle_ipi);
}

/// Completes local invalidation without locks, allocation or waiting.
///
/// Common IPI publishes acknowledgement only when decoding and every flush have completed.
fn handle_ipi(generation: u64) -> bool {
    let Some(mut requests) = REQUEST.decode(generation) else {
        return false;
    };
    requests.flush_with(&DynTlbFlusher::new::<LocalTlbFlusher>());
    true
}

/// Waits for the same exact generation on every captured remote CPU.
///
/// The caller holds only the mapping transaction guard and has finished local mutation/flush.
/// Empty request batches and target snapshots need no publication or interprocessor interrupts.
pub(crate) fn shootdown(
    targets: &[LogicalCpuId],
    requests: &TlbFlushes<VirtAddr>,
) -> Result<(), TlbShootdownError> {
    if matches!(requests, TlbFlushes::None)
        || matches!(requests, TlbFlushes::Pages(pages) if pages.is_empty())
    {
        return Ok(());
    }
    validate(requests, vmm::page_size())?;
    if targets.contains(&mp::current_cpu_id()) {
        return Err(TlbShootdownError::LocalTarget);
    }
    if targets.is_empty() {
        return Ok(());
    }
    ipi::call_many(
        targets,
        ipi::IpiReason::TlbShootdown,
        Duration::from_millis(500),
        |generation| REQUEST.publish(generation, requests),
    )
    .map_err(TlbShootdownError::Ipi)
}

#[cfg(test)]
mod tests {
    //! Request validation and IPI payload tests.

    use core::sync::atomic::Ordering;

    use expt::pt::cleanup::{SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlushes};
    use memory_addr::va;

    use crate::mp::LogicalCpuId;

    use super::{Payload, remote_targets, shootdown, validate};

    /// Empty requests need no initialized VMM, CPU inventory, or IPI transport.
    #[test]
    fn empty_batches_do_not_send_ipis() {
        assert!(shootdown(&[], &TlbFlushes::None).is_ok());
        assert!(shootdown(&[], &TlbFlushes::Pages(Default::default())).is_ok());
    }

    /// Rejects unaligned pages for every supported runtime granule.
    #[test]
    fn validates_runtime_granules() {
        for size in [4096, 16384, 65536] {
            let mut requests = TlbFlushes::None + TlbFlush::Page(va!(size));
            requests += TlbFlush::Page(va!(size * 3));
            assert!(validate(&requests, size).is_ok());
            requests += TlbFlush::Page(va!(size + 1));
            assert!(validate(&requests, size).is_err());
            assert!(validate(&TlbFlushes::None, size).is_ok());
            assert!(validate(&TlbFlushes::Full, size).is_ok());
        }
        assert!(validate(&TlbFlushes::Full, 0).is_err());
        assert!(validate(&TlbFlushes::None, 3).is_err());
    }

    /// Excludes the local CPU from an already filtered common snapshot.
    #[test]
    fn excludes_current_cpu() {
        assert_eq!(
            remote_targets(
                vec![
                    LogicalCpuId::new(0),
                    LogicalCpuId::new(2),
                    LogicalCpuId::new(3)
                ],
                LogicalCpuId::new(2),
            ),
            vec![LogicalCpuId::new(0), LogicalCpuId::new(3)]
        );
    }

    /// Preserves a full-capacity noncontiguous page list and rejects stale generations.
    #[test]
    fn payload_preserves_pages_and_requires_exact_generation() {
        let payload = Payload::new();
        assert!(payload.decode(0).is_none());
        assert!(payload.decode(1).is_none());
        let mut requests = TlbFlushes::None;
        for index in 0..SMALL_FLUSH_THRESHOLD {
            requests += TlbFlush::Page(va!(0x1000 + index * 0x3000));
        }
        payload.publish(1, &requests);
        let Some(TlbFlushes::Pages(pages)) = payload.decode(1) else {
            panic!("page-list payload lost");
        };
        assert_eq!(pages.len(), SMALL_FLUSH_THRESHOLD);
        for (index, address) in pages.into_iter().enumerate() {
            assert_eq!(address, va!(0x1000 + index * 0x3000));
        }
        assert!(payload.decode(2).is_none());

        payload.publish(2, &(TlbFlushes::None + TlbFlush::Page(va!(0x7000))));
        let Some(TlbFlushes::Pages(pages)) = payload.decode(2) else {
            panic!("single-page payload lost");
        };
        assert_eq!(pages.as_slice(), &[va!(0x7000)]);
        assert!(payload.decode(1).is_none());
        payload.publish(3, &TlbFlushes::Full);
        assert!(matches!(payload.decode(3), Some(TlbFlushes::Full)));
        assert!(payload.decode(2).is_none());
        payload.publish(4, &TlbFlushes::None);
        assert!(matches!(payload.decode(4), Some(TlbFlushes::None)));
        assert!(payload.decode(3).is_none());
    }

    /// Never accepts fields belonging to a concurrently replaced generation.
    #[test]
    fn payload_replacement_does_not_mix_page_lists() {
        let payload = Payload::new();
        std::thread::scope(|scope| {
            let shared = &payload;
            let writer = scope.spawn(move || {
                for generation in 1..=2000 {
                    let mut requests = TlbFlushes::None;
                    for index in 0..SMALL_FLUSH_THRESHOLD {
                        requests +=
                            TlbFlush::Page(va!(generation as usize * 0x100000 + index * 0x1000));
                    }
                    shared.publish(generation, &requests);
                }
            });
            for _ in 0..2000 {
                let generation = payload.generation.load(Ordering::SeqCst);
                if let Some(TlbFlushes::Pages(pages)) = payload.decode(generation) {
                    assert_eq!(pages.len(), SMALL_FLUSH_THRESHOLD);
                    for (index, address) in pages.iter().enumerate() {
                        assert_eq!(
                            *address,
                            va!(generation as usize * 0x100000 + index * 0x1000)
                        );
                    }
                }
            }
            writer.join().unwrap();
        });
        assert!(payload.decode(2000).is_some());
    }
}
