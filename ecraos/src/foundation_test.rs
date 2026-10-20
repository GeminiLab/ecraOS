//! Deterministic hardware foundation test operations.
//!
//! Dedicated smoke applications use semantic operations and counters without controller access.

use alloc::vec::Vec;
use core::time::Duration;

use exarch::mem::LocalTlbFlusher;
use expt::{pt::cleanup::TlbFlusher, pte::MappingFlags};
use memory_addr::{AddrRangeBounds, PhysAddr, VirtAddr, VirtAddrRange};

use crate::{
    ipi,
    mem::{
        allocs::{palloc, vmalloc},
        vmm,
    },
    mp::LogicalCpuId,
    task,
    tlb::{self, TlbShootdownError},
};

pub use ipi::{IpiDiagnostic, IpiError};
pub use tlb::TlbInvalidation as Invalidation;
pub use vmalloc::VMAllocError;

/// Returns the stable online and IPI-ready logical CPU snapshot.
///
/// CPU inventory ownership remains with MP and no test maintains a second target list.
pub fn ready_cpus() -> Vec<LogicalCpuId> {
    crate::mp::ipi_ready_cpu_ids()
}

/// Performs an exact-target Test round trip and returns its completed generation.
///
/// Self delivery executes directly while remote delivery uses the physical IPI mechanism.
pub fn test_roundtrip(target: LogicalCpuId, timeout: Duration) -> Result<u64, IpiError> {
    ipi::test_roundtrip(target, false, timeout)
}

/// Publishes Test and Reschedule together before one physical notification.
///
/// Both reasons survive the same mailbox drain and retain their independent semantics.
pub fn combined_roundtrip(target: LogicalCpuId, timeout: Duration) -> Result<u64, IpiError> {
    ipi::test_roundtrip(target, true, timeout)
}

/// Reads executed Test, consumed Reschedule and acknowledged Test generation counters.
///
/// Acquire observations allow applications to use counters as deterministic completion evidence.
pub fn counters(target: LogicalCpuId) -> (u64, u64, u64) {
    ipi::test_counters(target)
}

/// Suppresses exactly the next Test acknowledgement on one CPU.
///
/// The Test handler executes normally, but the sender must observe a bounded timeout.
pub fn suppress_test_ack(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::Test, true);
}

/// Clears any unused Test acknowledgement injection.
///
/// The following generation uses the normal production completion path.
pub fn clear_test_injection(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::Test, false);
}

/// Returns the active kernel page granule.
///
/// Diagnostic ranges follow the same runtime mode as production mappings.
pub fn page_size() -> usize {
    vmm::page_size()
}

/// Constructs the runtime-sized page range at an aligned address.
///
/// Request validation checks alignment before any mutation.
pub fn page_range(address: VirtAddr) -> VirtAddrRange {
    VirtAddrRange::from_start_size(address, page_size())
}

/// Allocates owned shared pages initialized with one value per page.
///
/// The guarded reservation is visible to readers only after normal mapping completion.
pub fn allocate_shared(values: &[u64]) -> Result<VirtAddrRange, VMAllocError> {
    let range = vmalloc::alloc_range_and_map_alloc(
        values.len(),
        1,
        MappingFlags::READ.union(MappingFlags::WRITE),
    )?;
    for (index, &value) in values.iter().enumerate() {
        // SAFETY: The new owned reservation is mapped and has not yet been published to readers.
        unsafe {
            (range.start + index * page_size())
                .as_mut_ptr_of::<u64>()
                .write_volatile(value);
        }
    }
    Ok(range)
}

/// Replaces barrier-protected shared pages with distinct physical backing.
///
/// The chosen Page, Range or All request completes for all captured remote targets after breaking
/// the old mapping and again after publishing its replacement. The returned generation identifies
/// the replacement's completion.
pub fn replace_shared(
    range: VirtAddrRange,
    values: &[u64],
    request: Invalidation,
) -> Result<u64, VMAllocError> {
    vmalloc::replace_for_foundation(range, values, request)
}

/// Verifies that every selected CPU acknowledged exactly one generation.
///
/// The observations read the production common TLB slots directly.
pub fn assert_tlb_completed(targets: &[LogicalCpuId], generation: u64) {
    assert_ne!(generation, 0);
    for &cpu in targets {
        assert_eq!(ipi::tlb_generation(cpu), generation, "TLB ack cpu={cpu}");
    }
}

/// Invalidates all local translations and completes every ready remote CPU.
///
/// This uses the same transaction, request payload and common acknowledgement as mapping changes.
pub fn flush_all() -> Result<u64, VMAllocError> {
    let transaction = vmm::MappingTransaction::begin()?;
    LocalTlbFlusher::flush(None);
    transaction.complete(Invalidation::All)?;
    Ok(tlb::published_generation())
}

/// Collects the currently live shared backing frame addresses.
///
/// Pending and quarantined mappings cannot be reported as live resources.
pub fn backing_frames(range: VirtAddrRange) -> Vec<PhysAddr> {
    range
        .iter(page_size())
        .expect("aligned shared range")
        .map(|address| vmalloc::virt_to_phys(address).expect("live shared backing"))
        .collect()
}

/// Removes a complete shared reservation with production completion semantics.
///
/// Timeout returns an error only after retaining the physical and virtual resources in quarantine.
pub fn deallocate_shared(address: VirtAddr) -> Result<(), VMAllocError> {
    vmalloc::dealloc_range(address)
}

/// Extracts common IPI diagnostics from a mapping completion failure.
///
/// Non-completion failures remain distinguishable and cannot masquerade as an injected timeout.
pub fn completion_ipi_error(error: &VMAllocError) -> Option<IpiError> {
    match error {
        VMAllocError::Completion(vmalloc::CompletionDiagnostic(TlbShootdownError::Ipi(error))) => {
            Some(*error)
        }
        _ => None,
    }
}

/// Reports whether the complete reservation still covers an address.
///
/// Applications can check both guard pages as well as the usable span after timeout.
pub fn reservation_retained(address: VirtAddr) -> bool {
    vmalloc::reservation_state(address).0
}

/// Reports whether a retained reservation is permanently quarantined.
///
/// A timed-out operation cannot restore the previous live mapping state.
pub fn reservation_quarantined(address: VirtAddr) -> bool {
    vmalloc::reservation_state(address).1
}

/// Reports that mapping metadata no longer describes a usable PTE.
///
/// This checks the production translator after local removal and completion failure.
pub fn backing_not_live(address: VirtAddr) -> bool {
    vmalloc::virt_to_phys(address).is_none()
}

/// Reads whether a physical frame remains allocated.
///
/// Quarantine must keep this true even though no live virtual mapping is reported.
pub fn frame_allocated(address: PhysAddr) -> bool {
    palloc::is_allocated(address).unwrap()
}

/// Attempts exact physical reuse and releases the probe allocation on success.
///
/// A quarantined frame must reject this allocator operation.
pub fn claim_released_frame(address: PhysAddr) -> bool {
    if palloc::alloc_frames_at(address, 1).is_err() {
        return false;
    }
    palloc::dealloc_frames_at(address, 1).unwrap();
    true
}

/// Attempts normal mapping into a quarantined reservation.
///
/// The production allocator must reject it before any new PTE or backing allocation.
pub fn remap_quarantined(range: VirtAddrRange) -> Result<(), VMAllocError> {
    vmalloc::map_alloc(range, MappingFlags::READ.union(MappingFlags::WRITE))
}

/// Suppresses exactly the next common TLB acknowledgement on one CPU.
///
/// The real local invalidation still executes, isolating completion and resource retention.
pub fn suppress_tlb_ack(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::TlbShootdown, true);
}

/// Clears an unused TLB completion fault injection.
///
/// The following generation uses the normal exact acknowledgement path.
pub fn clear_tlb_injection(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::TlbShootdown, false);
}

/// Returns the current task's usable stack allocation start.
///
/// Applications record this before exit and compare it only after safe-context reclamation.
pub fn current_stack_start() -> VirtAddr {
    task::current_task().unwrap().stack_start()
}

/// Reports whether a reader's stack is still used by an execution continuation.
///
/// The incoming scheduler clears this marker only after moving to a different stack.
pub fn task_stack_current(task: &task::TaskRef) -> bool {
    task.is_current()
}

/// Returns a retained reader's usable stack start.
///
/// The driver waits for this reservation to be released before comparing later reuse.
pub fn task_stack_start(task: &task::TaskRef) -> VirtAddr {
    task.stack_start()
}

/// Reclaims one batch of exited stacks from an ordinary task-context safe point.
///
/// Each reservation becomes reusable only through high-level acknowledged deallocation.
pub fn reap_stacks() {
    task::reap_exited_stacks();
}
