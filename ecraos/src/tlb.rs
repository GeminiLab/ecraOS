//! Semantic cross-CPU translation invalidation.
//!
//! Requests reuse common exact-generation acknowledgement and the MP ready snapshot.

use alloc::vec::Vec;
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use exarch::mem::LocalTlbFlusher;
use expt::pt::cleanup::TlbFlusher;
use memory_addr::{AddrRangeBounds, MemoryAddr, VirtAddr, VirtAddrRange, va};

use crate::{
    ipi,
    mem::vmm,
    mp::{self, LogicalCpuId},
};

/// A semantic invalidation in the global kernel address space.
///
/// Page and range boundaries use the runtime-selected kernel granule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlbInvalidation {
    /// One runtime-sized page.
    ///
    /// The address must be aligned to the active granule.
    Page(VirtAddr),
    /// A nonempty half-open page range.
    ///
    /// Every runtime-sized page receives a local invalidation.
    Range(VirtAddrRange),
    /// All translations in the active kernel address space.
    ///
    /// No ASID or PCID lifetime is implied.
    All,
}

/// A rejected request or incomplete remote invalidation.
///
/// Common IPI errors preserve the target, generation and absolute deadline diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TlbShootdownError {
    /// An invalid runtime granule or request boundary.
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

/// Validates a request against the active page granule.
///
/// Empty, reversed and unaligned ranges cannot reach the interrupt handler.
pub(crate) fn validate(request: TlbInvalidation, size: usize) -> Result<(), TlbShootdownError> {
    let valid = size.is_power_of_two()
        && match request {
            TlbInvalidation::Page(address) => address.is_aligned(size),
            TlbInvalidation::Range(range) => {
                range.start < range.end
                    && range.start.is_aligned(size)
                    && range.end.is_aligned(size)
            }
            TlbInvalidation::All => true,
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
    /// The page, range or full request discriminator.
    ///
    /// Values one through three encode the semantic variants.
    kind: AtomicUsize,
    /// The first virtual address at the atomic storage boundary.
    ///
    /// Full invalidation stores zero here.
    start: AtomicUsize,
    /// The exclusive range end at the atomic storage boundary.
    ///
    /// Page and full invalidations store zero here.
    end: AtomicUsize,
}

impl Payload {
    /// Creates unpublished atomic request storage.
    ///
    /// No generation can decode until the first complete publication.
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            kind: AtomicUsize::new(0),
            start: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// Publishes all fields before the common pending reasons become visible.
    ///
    /// The transaction lock gives this storage exactly one writer.
    fn publish(&self, generation: u64, request: TlbInvalidation) {
        self.generation.store(0, Ordering::SeqCst);
        let (kind, start, end) = match request {
            TlbInvalidation::Page(address) => (1, address.as_usize(), 0),
            TlbInvalidation::Range(range) => (2, range.start.as_usize(), range.end.as_usize()),
            TlbInvalidation::All => (3, 0, 0),
        };
        self.kind.store(kind, Ordering::SeqCst);
        self.start.store(start, Ordering::SeqCst);
        self.end.store(end, Ordering::SeqCst);
        self.generation.store(generation, Ordering::SeqCst);
    }

    /// Decodes only an unchanged exact-generation publication.
    ///
    /// A delayed interrupt cannot acknowledge a newer payload on behalf of an older request.
    fn decode(&self, generation: u64) -> Option<TlbInvalidation> {
        if generation == 0 || self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        let kind = self.kind.load(Ordering::SeqCst);
        let start = va!(self.start.load(Ordering::SeqCst));
        let end = va!(self.end.load(Ordering::SeqCst));
        if self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        match kind {
            1 => Some(TlbInvalidation::Page(start)),
            2 => Some(TlbInvalidation::Range(VirtAddrRange { start, end })),
            3 => Some(TlbInvalidation::All),
            _ => None,
        }
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
    let Some(request) = REQUEST.decode(generation) else {
        return false;
    };
    match request {
        TlbInvalidation::Page(address) => LocalTlbFlusher::flush(Some(address)),
        TlbInvalidation::Range(range) => {
            for address in range.iter(vmm::page_size()).expect("validated TLB range") {
                LocalTlbFlusher::flush(Some(address));
            }
        }
        TlbInvalidation::All => LocalTlbFlusher::flush(None),
    }
    true
}

/// Waits for the same exact generation on every captured remote CPU.
///
/// The caller holds only the mapping transaction guard and has finished local mutation/flush.
/// Empty boot snapshots need no publication and permit early local-only mapping operations.
pub(crate) fn shootdown(
    targets: &[LogicalCpuId],
    request: TlbInvalidation,
) -> Result<(), TlbShootdownError> {
    validate(request, vmm::page_size())?;
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
        |generation| REQUEST.publish(generation, request),
    )
    .map_err(TlbShootdownError::Ipi)
}

/// Semantic invalidation and payload tests.
///
/// Privileged instructions are replaced by pure request observations on the host.
#[cfg(test)]
mod tests {
    use memory_addr::{VirtAddrRange, va};

    use super::{Payload, TlbInvalidation, remote_targets, validate};

    /// Rejects invalid runtime-sized page and range requests.
    ///
    /// The same validation handles 4 KiB, 16 KiB and 64 KiB kernels.
    #[test]
    fn validates_runtime_granules() {
        for size in [4096, 16384, 65536] {
            assert!(validate(TlbInvalidation::Page(va!(size)), size).is_ok());
            assert!(validate(TlbInvalidation::Page(va!(size + 1)), size).is_err());
            assert!(
                validate(
                    TlbInvalidation::Range(VirtAddrRange {
                        start: va!(size),
                        end: va!(size)
                    }),
                    size
                )
                .is_err()
            );
            assert!(
                validate(
                    TlbInvalidation::Range(VirtAddrRange {
                        start: va!(size * 2),
                        end: va!(size)
                    }),
                    size
                )
                .is_err()
            );
            assert!(
                validate(
                    TlbInvalidation::Range(VirtAddrRange {
                        start: va!(size),
                        end: va!(size * 3 + 1)
                    }),
                    size
                )
                .is_err()
            );
            assert!(
                validate(
                    TlbInvalidation::Range(VirtAddrRange::new(va!(size), va!(size * 3))),
                    size
                )
                .is_ok()
            );
        }
    }

    /// Excludes the local CPU from an already filtered common snapshot.
    ///
    /// Offline and not-ready membership remains the MP owner's tested responsibility.
    #[test]
    fn excludes_current_cpu() {
        assert_eq!(remote_targets(vec![0, 2, 3], 2), vec![0, 3]);
    }

    /// Round-trips all request forms only for the matching generation.
    ///
    /// A timed-out older handler cannot decode a newer publication as its own payload.
    #[test]
    fn payload_requires_exact_generation() {
        let payload = Payload::new();
        let requests = [
            TlbInvalidation::Page(va!(0x10000)),
            TlbInvalidation::Range(VirtAddrRange::new(va!(0x10000), va!(0x30000))),
            TlbInvalidation::All,
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let generation = index as u64 + 1;
            payload.publish(generation, request);
            assert_eq!(payload.decode(generation), Some(request));
            assert_eq!(payload.decode(generation + 1), None);
            if generation > 1 {
                assert_eq!(payload.decode(generation - 1), None);
            }
        }
    }
}
