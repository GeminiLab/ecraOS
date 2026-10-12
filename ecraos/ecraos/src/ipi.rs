//! Common IPI reason publication and exact-generation completion.
//!
//! Mailboxes and the delivery protocol belong to the kernel rather than architecture controllers.

use alloc::{boxed::Box, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use lazyinit::LazyInit;

use crate::mp::{self, CpuState, LogicalCpuId, PhysicalCpuId};

/// An independently published kernel IPI reason.
///
/// Test and TLB are synchronous single-flight reasons while Reschedule coalesces asynchronously.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiReason {
    /// A deterministic delivery probe.
    ///
    /// Completion increments only the selected CPU's counter.
    Test = 0,
    /// A deferred local scheduler request.
    ///
    /// The interrupt-exit safe point applies the current domain's policy.
    Reschedule = 1,
    /// A semantic local TLB invalidation.
    ///
    /// The registered consumer completes before acknowledgement publication.
    TlbShootdown = 2,
}

impl IpiReason {
    /// Returns the independent pending bit for this reason.
    ///
    /// Multiple reasons may be present in the same mailbox drain.
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// The diagnostic context of a rejected or incomplete IPI operation.
///
/// Missing physical or generation values identify failures before their respective resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiDiagnostic {
    /// The requested semantic operation.
    ///
    /// Hardware-specific encodings remain outside this context.
    pub operation: IpiReason,
    /// The logical target selected by the caller.
    ///
    /// This remains available even when validation fails.
    pub logical_cpu: LogicalCpuId,
    /// The resolved firmware destination.
    ///
    /// Invalid logical targets have no physical destination.
    pub physical_cpu: Option<PhysicalCpuId>,
    /// The one generation allocated for the synchronous operation.
    ///
    /// Validation and reservation failures occur before allocation.
    pub generation: Option<u64>,
    /// The absolute monotonic completion deadline.
    ///
    /// Asynchronous requests use zero because they do not wait.
    pub deadline: Duration,
    /// The monotonic observation at failure.
    ///
    /// Timeout observations are at or beyond the deadline.
    pub observed_at: Duration,
    /// The target's latest acknowledgement.
    ///
    /// It must equal the allocated generation to complete the request.
    pub acknowledged: u64,
}

/// A common IPI publication or completion failure.
///
/// No failure fabricates acknowledgement or removes a runnable task from its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiError {
    /// An unknown logical target.
    ///
    /// No reservation or delivery is performed.
    InvalidCpu(IpiDiagnostic),
    /// A target outside the Online lifecycle state.
    ///
    /// Starting, offline and failed CPUs cannot accept kernel notifications.
    Offline(IpiDiagnostic),
    /// A target without initialized IPI consumers.
    ///
    /// Readiness is checked independently of Online state.
    NotReady(IpiDiagnostic),
    /// An unsupported physical delivery mechanism.
    ///
    /// No hardware write was submitted.
    Unsupported(IpiDiagnostic),
    /// A physical target that the mechanism cannot encode.
    ///
    /// No hardware write was submitted.
    InvalidPhysicalTarget(IpiDiagnostic),
    /// A rejected physical notification.
    ///
    /// The pending bit and reservation are released to permit a later retry.
    Transport(IpiDiagnostic),
    /// An occupied synchronous reason slot.
    ///
    /// Existing ownership and payload remain unchanged.
    Busy(IpiDiagnostic),
    /// An incomplete exact-generation acknowledgement.
    ///
    /// Cleanup permits retry but does not claim that the old operation completed.
    Timeout(IpiDiagnostic),
    /// A repeated target in one synchronous operation.
    ///
    /// Validation rejects it before payload publication.
    DuplicateTarget(IpiDiagnostic),
    /// An asynchronous API used with a synchronous reason or the reverse.
    ///
    /// Callers must select the reason's publication contract.
    InvalidReason,
    /// A synchronous remote wait with local interrupts disabled.
    ///
    /// Waiting cannot make reciprocal IPI progress in this context.
    InterruptsDisabled,
}

/// A transport-independent target-validation failure.
///
/// The protocol attaches logical, physical, deadline and generation context before returning it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    /// An unknown logical destination.
    ///
    /// Validation performs no reservation for it.
    InvalidCpu,
    /// A discovered CPU that is not Online.
    ///
    /// Readiness alone cannot qualify it for delivery.
    Offline,
    /// A CPU with incomplete local IPI initialization.
    ///
    /// Online state alone cannot qualify it for delivery.
    NotReady,
    /// An unavailable physical mechanism.
    ///
    /// No notification was submitted.
    Unsupported,
    /// An unrepresentable physical destination.
    ///
    /// Validation precedes the hardware write.
    InvalidPhysicalTarget,
    /// A transport rejection before submission.
    ///
    /// Common state is cleared to permit retry.
    Transport,
}

impl Failure {
    /// Attaches protocol diagnostics to a boundary failure.
    ///
    /// The variants preserve whether validation or transport submission failed.
    fn contextualize(self, diagnostic: IpiDiagnostic) -> IpiError {
        match self {
            Self::InvalidCpu => IpiError::InvalidCpu(diagnostic),
            Self::Offline => IpiError::Offline(diagnostic),
            Self::NotReady => IpiError::NotReady(diagnostic),
            Self::Unsupported => IpiError::Unsupported(diagnostic),
            Self::InvalidPhysicalTarget => IpiError::InvalidPhysicalTarget(diagnostic),
            Self::Transport => IpiError::Transport(diagnostic),
        }
    }
}

/// One independent single-flight synchronous reason slot.
///
/// The publisher owns reservation cleanup and the consumer publishes only an exact acknowledgement.
struct SyncSlot {
    /// The publisher's exclusive reservation.
    ///
    /// Timeout releases it without manufacturing completion.
    in_flight: AtomicBool,
    /// The payload generation published before its pending bit.
    ///
    /// Acquire readers cannot observe it before preceding payload writes.
    generation: AtomicU64,
    /// The latest completed generation.
    ///
    /// Monotonic updates prevent a late old handler from erasing a newer acknowledgement.
    acknowledged: AtomicU64,
    /// The latest generation claimed by a consumer.
    ///
    /// This prevents a captured old reason bit from executing a retry twice.
    claimed: AtomicU64,
}

impl SyncSlot {
    /// Creates an idle synchronous slot.
    ///
    /// Generation zero never denotes a published request.
    const fn new() -> Self {
        Self {
            in_flight: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
            claimed: AtomicU64::new(0),
        }
    }
    /// Reserves an idle slot without replacing an in-flight request.
    ///
    /// Acquire ordering pairs with the preceding publisher's cleanup.
    fn reserve(&self) -> bool {
        self.in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    /// Publishes the operation generation after its payload.
    ///
    /// A captured pending bit may safely observe a newer generation only after these writes.
    fn publish_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }
    /// Claims a live generation once for local execution.
    ///
    /// Stale captured bits cannot duplicate an operation or acknowledge an unpublished generation.
    fn claim(&self) -> Option<u64> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == 0 || !self.in_flight.load(Ordering::Acquire) {
            return None;
        }
        (self.claimed.fetch_max(generation, Ordering::AcqRel) < generation).then_some(generation)
    }
    /// Publishes completion without regressing a later acknowledgement.
    ///
    /// Exact equality remains necessary in the waiter even when a stale consumer finishes late.
    fn acknowledge(&self, generation: u64) {
        self.acknowledged.fetch_max(generation, Ordering::Release);
    }
    /// Observes whether this exact generation completed.
    ///
    /// Acquire ordering makes the reason consumer's effects visible to the caller.
    fn is_acknowledged(&self, generation: u64) -> bool {
        self.acknowledged.load(Ordering::Acquire) == generation
    }
    /// Releases the publisher's exclusive reservation.
    ///
    /// The caller clears its pending reason before allowing the next reservation.
    fn release(&self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

/// One CPU's independent reasons and synchronous completion slots.
///
/// The consumer drains without allocation, waiting or subsystem locks.
struct Mailbox {
    /// The independent pending bits and asynchronous publication stamp.
    ///
    /// Release publication pairs with the consumer's AcqRel drain.
    /// The upper bits retain a monotonic stamp through drain, allowing a rejected sender to clear
    /// only its own Reschedule publication without erasing a successful newer sender.
    pending: AtomicU64,
    /// The single-flight Test slot.
    ///
    /// It can coexist with a TLB request and a Reschedule bit.
    test: SyncSlot,
    /// The single-flight TLB slot.
    ///
    /// Mapping policy supplies the semantic payload separately.
    tlb: SyncSlot,
    /// The number of executed Test generations.
    ///
    /// Counters identify exact hardware targets in the smoke application.
    test_count: AtomicU64,
    /// The number of consumed Reschedule publications.
    ///
    /// Diagnostics can observe combined reasons without exposing mailbox storage.
    reschedule_count: AtomicU64,
    /// The once-only acknowledgement suppression for foundation tests.
    ///
    /// Production builds have no injection state or branch.
    #[cfg(any(test, feature = "foundation-test"))]
    suppress_ack: core::sync::atomic::AtomicU8,
}

impl Mailbox {
    /// Creates an empty CPU mailbox.
    ///
    /// All counters and generation slots begin idle.
    const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            test: SyncSlot::new(),
            tlb: SyncSlot::new(),
            test_count: AtomicU64::new(0),
            reschedule_count: AtomicU64::new(0),
            #[cfg(any(test, feature = "foundation-test"))]
            suppress_ack: core::sync::atomic::AtomicU8::new(0),
        }
    }
    /// Selects the independent synchronous slot for a reason.
    ///
    /// Reschedule has no generation or acknowledgement reservation.
    fn slot(&self, reason: IpiReason) -> Option<&SyncSlot> {
        match reason {
            IpiReason::Test => Some(&self.test),
            IpiReason::TlbShootdown => Some(&self.tlb),
            IpiReason::Reschedule => None,
        }
    }
    /// Publishes a pending reason and reports its clear-to-pending transition.
    ///
    /// A repeated asynchronous publication coalesces without another physical notification.
    fn publish(&self, reason: IpiReason) -> bool {
        if reason == IpiReason::Reschedule {
            return self.publish_reschedule().is_some();
        }
        self.pending
            .fetch_or(u64::from(reason.bit()), Ordering::Release)
            & u64::from(reason.bit())
            == 0
    }
    /// Publishes a coalesced reason with an atomic ownership stamp.
    ///
    /// Only a clear-to-pending transition allocates a stamp and requires physical submission.
    fn publish_reschedule(&self) -> Option<u64> {
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                if pending & u64::from(IpiReason::Reschedule.bit()) != 0 {
                    return None;
                }
                let stamp = (pending >> 3)
                    .checked_add(1)
                    .filter(|&value| value < (1 << 61))
                    .expect("reschedule publication stamp exhausted");
                Some((stamp << 3) | (pending & 7) | u64::from(IpiReason::Reschedule.bit()))
            })
            .ok()
            .map(|previous| (previous >> 3) + 1)
    }
    /// Cancels only the rejected sender's stamped asynchronous publication.
    ///
    /// A concurrent drain and newer successful send retain the newer pending bit atomically.
    fn cancel_reschedule(&self, stamp: u64) {
        let _ = self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending >> 3 == stamp).then_some(pending & !u64::from(IpiReason::Reschedule.bit()))
            });
    }
    /// Clears a rejected or retired publisher's pending reason.
    ///
    /// Other independent reason bits remain available to the consumer.
    fn clear(&self, reason: IpiReason) {
        self.pending
            .fetch_and(!u64::from(reason.bit()), Ordering::AcqRel);
    }
    /// Executes every pending reason and repeats for publications during execution.
    ///
    /// The callback returns whether synchronous consumer execution completed successfully.
    fn drain(&self, mut handle: impl FnMut(IpiReason, u64) -> bool) {
        loop {
            // Preserve the asynchronous stamp while atomically draining all independent bits.
            let pending = (self.pending.fetch_and(!7, Ordering::AcqRel) & 7) as u8;
            if pending == 0 {
                break;
            }
            for reason in [
                IpiReason::Test,
                IpiReason::Reschedule,
                IpiReason::TlbShootdown,
            ] {
                if pending & reason.bit() == 0 {
                    continue;
                }
                let slot = self.slot(reason);
                let generation = match slot {
                    Some(slot) => match slot.claim() {
                        Some(generation) => generation,
                        None => continue,
                    },
                    None => 0,
                };
                match reason {
                    IpiReason::Test => {
                        self.test_count.fetch_add(1, Ordering::Relaxed);
                    }
                    IpiReason::Reschedule => {
                        self.reschedule_count.fetch_add(1, Ordering::Release);
                    }
                    IpiReason::TlbShootdown => {}
                }
                let completed = handle(reason, generation);
                #[cfg(any(test, feature = "foundation-test"))]
                let completed = completed
                    && self.suppress_ack.fetch_and(!reason.bit(), Ordering::AcqRel) & reason.bit()
                        == 0;
                if completed && let Some(slot) = slot {
                    slot.acknowledge(generation);
                }
            }
        }
    }
}

/// The physical-delivery, logical-validation and monotonic-clock boundary.
///
/// Host tests replace only these external mechanisms and retain the real protocol state machine.
trait Backend {
    /// Returns the common mailbox for a target.
    ///
    /// Invalid logical IDs return `None`.
    fn mailbox(&self, target: LogicalCpuId) -> Option<&Mailbox>;
    /// Returns the current logical CPU.
    ///
    /// Self delivery executes directly without a hardware notification.
    fn current_cpu(&self) -> LogicalCpuId;
    /// Validates readiness and resolves a firmware destination.
    ///
    /// Resolution remains in the kernel inventory owner.
    fn validate(&self, target: LogicalCpuId) -> Result<PhysicalCpuId, Failure>;
    /// Resolves any known physical ID for rejected-target diagnostics.
    ///
    /// Runtime inventory lookup retains the ID even when lifecycle validation rejects delivery.
    fn physical_id(&self, target: LogicalCpuId) -> Option<PhysicalCpuId> {
        self.validate(target).ok()
    }
    /// Allocates one nonzero globally unique generation.
    ///
    /// Each multi-target operation invokes this once after all reservations succeed.
    fn next_generation(&self) -> u64;
    /// Submits a physical notification.
    ///
    /// An error means no write was initiated and permits publisher cleanup.
    fn send(&self, target: LogicalCpuId, physical: PhysicalCpuId) -> Result<(), Failure>;
    /// Returns the current monotonic observation.
    ///
    /// Synchronous waits compare this against one absolute deadline.
    fn now(&self) -> Duration;
    /// Executes a self-target mailbox directly.
    ///
    /// Reason consumers obey the same allocation-free contract as interrupt dispatch.
    fn execute(&self, target: LogicalCpuId);
}

/// Builds the complete diagnostic for one protocol observation.
///
/// Acknowledgement storage is read only when a synchronous slot exists.
fn diagnostic(
    backend: &impl Backend,
    target: LogicalCpuId,
    reason: IpiReason,
    physical: Option<PhysicalCpuId>,
    generation: Option<u64>,
    deadline: Duration,
) -> IpiDiagnostic {
    IpiDiagnostic {
        operation: reason,
        logical_cpu: target,
        physical_cpu: physical.or_else(|| backend.physical_id(target)),
        generation,
        deadline,
        observed_at: backend.now(),
        acknowledged: backend
            .mailbox(target)
            .and_then(|m| m.slot(reason))
            .map_or(0, |slot| slot.acknowledged.load(Ordering::Acquire)),
    }
}

/// Publishes one asynchronous notification through the real common protocol.
///
/// Failed submission clears the coalescing bit so subsequent queue publication can retry.
fn notify_inner(
    backend: &impl Backend,
    target: LogicalCpuId,
    reason: IpiReason,
) -> Result<(), IpiError> {
    if reason != IpiReason::Reschedule {
        return Err(IpiError::InvalidReason);
    }
    let physical = backend.validate(target).map_err(|failure| {
        failure.contextualize(diagnostic(
            backend,
            target,
            reason,
            None,
            None,
            Duration::ZERO,
        ))
    })?;
    let mailbox = backend
        .mailbox(target)
        .expect("validated target has no mailbox");
    let Some(stamp) = mailbox.publish_reschedule() else {
        return Ok(());
    };
    if target == backend.current_cpu() {
        backend.execute(target);
        return Ok(());
    }
    if let Err(failure) = backend.send(target, physical) {
        mailbox.cancel_reschedule(stamp);
        return Err(failure.contextualize(diagnostic(
            backend,
            target,
            reason,
            Some(physical),
            None,
            Duration::ZERO,
        )));
    }
    Ok(())
}

/// The exclusive reservations owned by one multi-target operation.
///
/// Every exit path clears pending state before permitting a newer publisher.
struct Reservations<'a> {
    /// The already validated target mailboxes and physical destinations.
    ///
    /// This owned list is prepared before payload publication.
    targets: Vec<(LogicalCpuId, PhysicalCpuId, &'a Mailbox)>,
    /// The one synchronous reason reserved by this operation.
    ///
    /// Independent reasons are never cleared during cleanup.
    reason: IpiReason,
}

impl Drop for Reservations<'_> {
    /// Clears pending state and releases all owned synchronous slots.
    ///
    /// Busy, transport failure and timeout have the same reservation cleanup guarantee.
    fn drop(&mut self) {
        for &(_, _, mailbox) in &self.targets {
            mailbox.clear(self.reason);
            mailbox.slot(self.reason).unwrap().release();
        }
    }
}

/// Publishes one synchronous generation to an entirely validated target set.
///
/// All targets are reserved before the once-only payload callback and all sends precede any wait.
fn call_many_inner(
    backend: &impl Backend,
    targets: &[LogicalCpuId],
    reason: IpiReason,
    deadline: Duration,
    publish_payload: impl FnOnce(u64),
) -> Result<(), IpiError> {
    if reason == IpiReason::Reschedule {
        return Err(IpiError::InvalidReason);
    }
    let mut validated = Vec::with_capacity(targets.len());
    for (index, &target) in targets.iter().enumerate() {
        if targets[..index].contains(&target) {
            return Err(IpiError::DuplicateTarget(diagnostic(
                backend, target, reason, None, None, deadline,
            )));
        }
        let physical = backend.validate(target).map_err(|failure| {
            failure.contextualize(diagnostic(backend, target, reason, None, None, deadline))
        })?;
        validated.push((
            target,
            physical,
            backend
                .mailbox(target)
                .expect("validated target has no mailbox"),
        ));
    }
    let mut reservations = Reservations {
        targets: Vec::with_capacity(targets.len()),
        reason,
    };
    for (target, physical, mailbox) in validated {
        if !mailbox.slot(reason).unwrap().reserve() {
            return Err(IpiError::Busy(diagnostic(
                backend,
                target,
                reason,
                Some(physical),
                None,
                deadline,
            )));
        }
        reservations.targets.push((target, physical, mailbox));
    }
    let generation = backend.next_generation();
    publish_payload(generation);
    for &(_, _, mailbox) in &reservations.targets {
        mailbox.slot(reason).unwrap().publish_generation(generation);
        mailbox.publish(reason);
    }
    let mut send_failure = None;
    for &(target, physical, _) in &reservations.targets {
        if target == backend.current_cpu() {
            backend.execute(target);
        } else if let Err(failure) = backend.send(target, physical) {
            // Keep submitting other targets before observing completion or returning failure.
            send_failure.get_or_insert((target, physical, failure));
        }
    }
    if let Some((target, physical, failure)) = send_failure {
        return Err(failure.contextualize(diagnostic(
            backend,
            target,
            reason,
            Some(physical),
            Some(generation),
            deadline,
        )));
    }
    for &(target, physical, mailbox) in &reservations.targets {
        while !mailbox.slot(reason).unwrap().is_acknowledged(generation) {
            let observed = backend.now();
            // An acknowledgement may have arrived during the clock observation.
            if mailbox.slot(reason).unwrap().is_acknowledged(generation) {
                break;
            }
            if observed >= deadline {
                return Err(IpiError::Timeout(diagnostic(
                    backend,
                    target,
                    reason,
                    Some(physical),
                    Some(generation),
                    deadline,
                )));
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// The one kernel-owned mailbox per discovered logical CPU.
///
/// Allocation precedes local source enabling and every IPI-ready publication.
static MAILBOXES: LazyInit<Box<[Mailbox]>> = LazyInit::new();

/// The global nonzero synchronous generation sequence.
///
/// Generations never wrap or repeat, including after transport failure and timeout.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The once-installed semantic TLB consumer.
///
/// Task 10 installs an allocation-free function before CPUs become ready.
static TLB_HANDLER: LazyInit<fn(u64) -> bool> = LazyInit::new();

/// The live kernel delivery and inventory boundary.
///
/// Mailbox ownership remains common while physical submission belongs to exarch.
struct KernelBackend;

impl Backend for KernelBackend {
    /// Returns one initialized common mailbox.
    ///
    /// Discovery bounds and pre-initialization calls are checked without panicking.
    fn mailbox(&self, target: LogicalCpuId) -> Option<&Mailbox> {
        MAILBOXES.get()?.get(target)
    }
    /// Returns the caller's stable logical identifier.
    ///
    /// Synchronous wrappers prevent migration while leaving interrupts enabled.
    fn current_cpu(&self) -> LogicalCpuId {
        mp::current_cpu_id()
    }
    /// Validates lifecycle and readiness through the MP owner.
    ///
    /// Online and IPI-ready remain independent conditions.
    fn validate(&self, target: LogicalCpuId) -> Result<PhysicalCpuId, Failure> {
        let physical = mp::logical_to_physical(target).ok_or(Failure::InvalidCpu)?;
        if mp::cpu_state(target) != Some(CpuState::Online) {
            return Err(Failure::Offline);
        }
        if !mp::is_ipi_ready(target) || self.mailbox(target).is_none() {
            return Err(Failure::NotReady);
        }
        Ok(physical)
    }
    /// Returns a discovered physical ID independently of readiness.
    ///
    /// Failure diagnostics retain IDs for offline and not-ready CPUs.
    fn physical_id(&self, target: LogicalCpuId) -> Option<PhysicalCpuId> {
        mp::logical_to_physical(target)
    }
    /// Allocates a globally unique synchronous generation.
    ///
    /// Exhaustion is fatal rather than permitting stale-generation aliasing.
    fn next_generation(&self) -> u64 {
        NEXT_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("IPI generation space exhausted")
    }
    /// Submits one physical interrupt through the architecture mechanism.
    ///
    /// Errors guarantee that no hardware notification was initiated.
    fn send(&self, _target: LogicalCpuId, physical: PhysicalCpuId) -> Result<(), Failure> {
        exarch::ipi::send(physical).map_err(|error| match error {
            exarch::ipi::IpiError::Unsupported => Failure::Unsupported,
            exarch::ipi::IpiError::InvalidTarget => Failure::InvalidPhysicalTarget,
            exarch::ipi::IpiError::Transport => Failure::Transport,
        })
    }
    /// Observes the architecture's monotonic clock.
    ///
    /// Waiters compare observations with one absolute deadline.
    fn now(&self) -> Duration {
        exarch::time::monotonic_time()
    }
    /// Executes reasons directly on the self target.
    ///
    /// No hardware IPI is submitted and no subsystem lock is acquired by reason dispatch.
    fn execute(&self, target: LogicalCpuId) {
        drain_mailbox(&MAILBOXES[target]);
    }
}

/// Allocates common mailboxes after CPU discovery.
///
/// The BSP calls this before enabling any local software source.
pub(crate) fn init() {
    MAILBOXES.init_once(
        (0..mp::cpu_count())
            .map(|_| Mailbox::new())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
}

/// Installs the one allocation-free TLB reason consumer.
///
/// Initialization is once-only and precedes CPU readiness, so handlers need no registration lock.
pub(crate) fn register_tlb_handler(handler: fn(u64) -> bool) {
    assert!(
        TLB_HANDLER.get().is_none(),
        "TLB IPI consumer already registered"
    );
    TLB_HANDLER.init_once(handler);
}

/// Drains semantic reasons without waiting or acquiring subsystem locks.
///
/// Scheduler notification only sets a local bit, and a TLB callback only executes local flushes.
fn drain_mailbox(mailbox: &Mailbox) {
    mailbox.drain(|reason, generation| match reason {
        IpiReason::Test => true,
        IpiReason::Reschedule => {
            crate::task::preempt::request_reschedule();
            true
        }
        IpiReason::TlbShootdown => TLB_HANDLER.get().is_some_and(|handler| handler(generation)),
    });
}

/// Handles the current CPU's completed software interrupt source.
///
/// The architecture clears SSIP or acknowledges its controller before entering this function.
pub(crate) fn handle_ipi() {
    if let Some(mailbox) = MAILBOXES
        .get()
        .and_then(|all| all.get(mp::current_cpu_id()))
    {
        drain_mailbox(mailbox);
    }
}

/// Publishes an asynchronous coalesced kernel reason.
///
/// Queue and domain locks must be released before physical submission.
pub(crate) fn notify(target: LogicalCpuId, reason: IpiReason) -> Result<(), IpiError> {
    let _guard = kernel_guard::NoPreempt::new();
    notify_inner(&KernelBackend, target, reason)
}

/// Publishes one synchronous payload and waits for every exact acknowledgement.
///
/// Callers hold no IRQ-saving, page-table, allocator or scheduler queue lock. The preemption guard
/// prevents self-target migration and leaves local interrupts available for reciprocal progress.
pub(crate) fn call_many(
    targets: &[LogicalCpuId],
    reason: IpiReason,
    timeout: Duration,
    publish_payload: impl FnOnce(u64),
) -> Result<(), IpiError> {
    if !exarch::trap::local_enabled() {
        return Err(IpiError::InterruptsDisabled);
    }
    let _guard = kernel_guard::NoPreempt::new();
    if reason == IpiReason::TlbShootdown && TLB_HANDLER.get().is_none() {
        return Err(IpiError::Unsupported(diagnostic(
            &KernelBackend,
            mp::current_cpu_id(),
            reason,
            mp::logical_to_physical(mp::current_cpu_id()),
            None,
            Duration::ZERO,
        )));
    }
    let deadline = exarch::time::monotonic_time().saturating_add(timeout);
    call_many_inner(&KernelBackend, targets, reason, deadline, publish_payload)
}

/// Performs one exact-generation Test round trip for a foundation application.
///
/// Combined requests publish both bits before any hardware send.
#[cfg(feature = "foundation-test")]
pub(crate) fn test_roundtrip(
    target: LogicalCpuId,
    combined: bool,
    timeout: Duration,
) -> Result<u64, IpiError> {
    let mut generation = 0;
    call_many(&[target], IpiReason::Test, timeout, |value| {
        generation = value;
        if combined {
            MAILBOXES[target].publish(IpiReason::Reschedule);
        }
    })?;
    Ok(generation)
}

/// Observes executed reasons and exact Test completion.
///
/// Foundation applications get counters rather than private mailbox references.
#[cfg(feature = "foundation-test")]
pub(crate) fn test_counters(target: LogicalCpuId) -> (u64, u64, u64) {
    let mailbox = &MAILBOXES[target];
    (
        mailbox.test_count.load(Ordering::Acquire),
        mailbox.reschedule_count.load(Ordering::Acquire),
        mailbox.test.acknowledged.load(Ordering::Acquire),
    )
}

/// Reads the common TLB slot's exact completion generation.
///
/// Foundation tests inspect production acknowledgement storage rather than maintaining counters.
#[cfg(feature = "foundation-test")]
pub(crate) fn tlb_generation(target: LogicalCpuId) -> u64 {
    MAILBOXES[target].tlb.acknowledged.load(Ordering::Acquire)
}

/// Suppresses or clears one synchronous acknowledgement for deterministic fault injection.
///
/// The consumer still executes, so timeout cannot be confused with a failed physical delivery.
#[cfg(feature = "foundation-test")]
pub(crate) fn suppress_next_ack(target: LogicalCpuId, reason: IpiReason, suppress: bool) {
    assert!(reason != IpiReason::Reschedule);
    let mailbox = &MAILBOXES[target];
    if suppress {
        mailbox
            .suppress_ack
            .fetch_or(reason.bit(), Ordering::Release);
    } else {
        mailbox
            .suppress_ack
            .fetch_and(!reason.bit(), Ordering::AcqRel);
    }
}

/// Atomic mailbox and transport-independent protocol tests.
///
/// Controlled transports exercise the production publication, drain and deadline logic.
#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use core::{
        cell::{Cell, RefCell},
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use super::{Backend, Failure, IpiError, IpiReason, Mailbox, call_many_inner, notify_inner};

    /// A deterministic clock and physical-delivery test boundary.
    ///
    /// Real mailboxes remain under test while privileged sends are replaced by local dispatch.
    struct Fake {
        /// The real per-target mailboxes.
        ///
        /// The fake never replaces their atomic state transitions.
        mailboxes: Vec<Mailbox>,
        /// The common monotonic generation allocator.
        ///
        /// Each operation receives one generation irrespective of target count.
        generations: AtomicU64,
        /// The deterministic clock in milliseconds.
        ///
        /// Every observation advances it without sleeping.
        clock: Cell<u64>,
        /// The submitted logical targets.
        ///
        /// The first clock observation asserts that all sends happened before any wait.
        sent: RefCell<Vec<usize>>,
        /// The number of sends required before waiting.
        ///
        /// Zero permits validation-only error tests.
        expected_sends: Cell<usize>,
        /// The target whose acknowledgement is omitted.
        ///
        /// Publication and delivery still execute for that target.
        missing: Cell<Option<usize>>,
        /// The target whose transport returns failure before submission.
        ///
        /// Other targets still receive their notifications.
        failed_send: Cell<Option<usize>>,
        /// The race injection for a new publication during an older failed send.
        ///
        /// The old reason is consumed and a retry succeeds before the old transport returns.
        replace_failed: Cell<bool>,
        /// The target rejected by lifecycle validation.
        ///
        /// The stored failure distinguishes invalid, offline and not-ready states.
        invalid: Cell<Option<(usize, Failure)>>,
    }

    impl Fake {
        /// Creates three real mailboxes with a deterministic delivery boundary.
        ///
        /// Logical CPU zero is the self target.
        fn new() -> Self {
            Self {
                mailboxes: (0..3).map(|_| Mailbox::new()).collect(),
                generations: AtomicU64::new(1),
                clock: Cell::new(0),
                sent: RefCell::new(Vec::new()),
                expected_sends: Cell::new(0),
                missing: Cell::new(None),
                failed_send: Cell::new(None),
                replace_failed: Cell::new(false),
                invalid: Cell::new(None),
            }
        }
    }

    impl Backend for Fake {
        /// Returns the real mailbox for a valid logical target.
        ///
        /// Out-of-range identifiers remain invalid.
        fn mailbox(&self, target: usize) -> Option<&Mailbox> {
            self.mailboxes.get(target)
        }
        /// Returns the executing logical CPU.
        ///
        /// Self-target tests must avoid the send boundary entirely.
        fn current_cpu(&self) -> usize {
            0
        }
        /// Validates a logical target and resolves its physical destination.
        ///
        /// Physical IDs deliberately differ from logical indices.
        fn validate(&self, target: usize) -> Result<usize, Failure> {
            if let Some((id, failure)) = self.invalid.get()
                && id == target
            {
                return Err(failure);
            }
            self.mailboxes
                .get(target)
                .map(|_| target + 10)
                .ok_or(Failure::InvalidCpu)
        }
        /// Allocates one globally unique generation.
        ///
        /// Multi-target operations must call this exactly once.
        fn next_generation(&self) -> u64 {
            self.generations.fetch_add(1, Ordering::Relaxed)
        }
        /// Submits a physical notification or rejects it before submission.
        ///
        /// Dispatch is delayed until the waiting phase to expose publication ordering bugs.
        fn send(&self, target: usize, physical: usize) -> Result<(), Failure> {
            assert_eq!(physical, target + 10);
            self.sent.borrow_mut().push(target);
            if self.failed_send.get() == Some(target) {
                if self.replace_failed.replace(false) {
                    self.mailboxes[target].drain(|_, _| true);
                    self.failed_send.set(None);
                    notify_inner(self, target, IpiReason::Reschedule).unwrap();
                }
                Err(Failure::Transport)
            } else {
                Ok(())
            }
        }
        /// Observes the bounded clock and completes all successfully delivered targets.
        ///
        /// No target may be waited on until every requested transport send has occurred.
        fn now(&self) -> Duration {
            if self.expected_sends.get() != 0 {
                assert_eq!(self.sent.borrow().len(), self.expected_sends.get());
                for &id in self.sent.borrow().iter() {
                    if self.failed_send.get() != Some(id) {
                        self.mailboxes[id].drain(|_, _| self.missing.get() != Some(id));
                    }
                }
            }
            let now = self.clock.get();
            self.clock.set(now + 1);
            Duration::from_millis(now)
        }
        /// Executes self-target reasons without hardware submission.
        ///
        /// This uses the same drain path as remote interrupt dispatch.
        fn execute(&self, target: usize) {
            self.mailboxes[target].drain(|_, _| true);
        }
    }

    /// Preserves simultaneous reasons while coalescing repeated asynchronous publication.
    ///
    /// Replacing a mailbox with a single enum would lose either Test or Reschedule.
    #[test]
    fn simultaneous_reasons_and_coalescing() {
        let mailbox = Mailbox::new();
        assert!(mailbox.publish(IpiReason::Reschedule));
        assert!(!mailbox.publish(IpiReason::Reschedule));
        assert!(mailbox.test.reserve());
        mailbox.test.publish_generation(7);
        mailbox.publish(IpiReason::Test);
        let mut seen = Vec::new();
        mailbox.drain(|reason, generation| {
            seen.push((reason, generation));
            true
        });
        assert_eq!(seen, vec![(IpiReason::Test, 7), (IpiReason::Reschedule, 0)]);
        assert!(mailbox.test.is_acknowledged(7));
        assert_eq!(mailbox.test_count.load(Ordering::Acquire), 1);
    }

    /// Drains reasons published during consumer execution.
    ///
    /// A one-shot swap would strand the newly published TLB reason.
    #[test]
    fn drain_repeats_for_publication_during_handler() {
        let mailbox = Mailbox::new();
        mailbox.publish(IpiReason::Reschedule);
        let mut seen = Vec::new();
        mailbox.drain(|reason, _| {
            seen.push(reason);
            if reason == IpiReason::Reschedule {
                assert!(mailbox.tlb.reserve());
                mailbox.tlb.publish_generation(9);
                mailbox.publish(IpiReason::TlbShootdown);
            }
            true
        });
        assert_eq!(seen, vec![IpiReason::Reschedule, IpiReason::TlbShootdown]);
        assert!(mailbox.tlb.is_acknowledged(9));
    }

    /// Rejects a second publisher and prevents stale acknowledgements from regressing completion.
    ///
    /// A late generation must neither complete nor erase a newer acknowledgement.
    #[test]
    fn busy_and_exact_generation() {
        let mailbox = Mailbox::new();
        assert!(mailbox.test.reserve());
        assert!(!mailbox.test.reserve());
        mailbox.test.publish_generation(7);
        mailbox.test.acknowledge(7);
        assert!(!mailbox.test.is_acknowledged(8));
        mailbox.test.release();
        assert!(mailbox.test.reserve());
        mailbox.test.publish_generation(8);
        mailbox.test.acknowledge(8);
        mailbox.test.acknowledge(7);
        assert!(mailbox.test.is_acknowledged(8));
    }

    /// Publishes one payload after reserving all targets and sends every target before waiting.
    ///
    /// Self-target execution is direct and shares the exact common generation.
    #[test]
    fn multi_target_publication_and_self_target() {
        let fake = Fake::new();
        fake.expected_sends.set(2);
        let publications = Cell::new(0);
        call_many_inner(
            &fake,
            &[0, 1, 2],
            IpiReason::Test,
            Duration::from_millis(10),
            |generation| {
                assert_eq!(generation, 1);
                publications.set(publications.get() + 1);
                assert!(fake.mailboxes.iter().all(|m| !m.test.reserve()));
                assert!(fake.sent.borrow().is_empty());
            },
        )
        .unwrap();
        assert_eq!(publications.get(), 1);
        assert_eq!(*fake.sent.borrow(), vec![1, 2]);
        assert!(fake.mailboxes.iter().all(|m| m.test.is_acknowledged(1)));
    }

    /// Rolls every reservation back when any target is invalid or already busy.
    ///
    /// Payload publication must never occur for a partially accepted target set.
    #[test]
    fn validation_and_busy_rollback() {
        for failure in [Failure::InvalidCpu, Failure::Offline, Failure::NotReady] {
            let fake = Fake::new();
            fake.invalid.set(Some((2, failure)));
            assert!(
                call_many_inner(
                    &fake,
                    &[1, 2],
                    IpiReason::Test,
                    Duration::from_millis(10),
                    |_| panic!("published invalid set")
                )
                .is_err()
            );
            assert!(fake.mailboxes[1].test.reserve());
            assert!(fake.sent.borrow().is_empty());
        }
        let fake = Fake::new();
        assert!(fake.mailboxes[2].test.reserve());
        assert!(matches!(
            call_many_inner(
                &fake,
                &[1, 2],
                IpiReason::Test,
                Duration::from_millis(10),
                |_| panic!("published busy set")
            ),
            Err(IpiError::Busy(_))
        ));
        assert!(fake.mailboxes[1].test.reserve());
    }

    /// Reports a missing target without manufacturing completion and permits the next generation.
    ///
    /// Acknowledgements from the timed-out generation cannot satisfy the retry.
    #[test]
    fn missing_ack_times_out_and_new_generation_recovers() {
        let fake = Fake::new();
        fake.expected_sends.set(2);
        fake.missing.set(Some(2));
        let error = call_many_inner(
            &fake,
            &[1, 2],
            IpiReason::Test,
            Duration::from_millis(3),
            |_| {},
        )
        .unwrap_err();
        let IpiError::Timeout(details) = error else {
            panic!("wrong failure: {error:?}")
        };
        assert_eq!(details.logical_cpu, 2);
        assert_eq!(details.physical_cpu, Some(12));
        assert_eq!(details.generation, Some(1));
        assert!(details.observed_at >= details.deadline);
        assert!(!fake.mailboxes[2].test.is_acknowledged(1));
        fake.mailboxes[2].test.acknowledge(1);
        fake.missing.set(None);
        fake.sent.borrow_mut().clear();
        fake.expected_sends.set(1);
        call_many_inner(
            &fake,
            &[2],
            IpiReason::Test,
            Duration::from_millis(30),
            |generation| assert_eq!(generation, 2),
        )
        .unwrap();
        assert!(fake.mailboxes[2].test.is_acknowledged(2));
    }

    /// Requires the exact common TLB generation on every remote target.
    ///
    /// Missing completion names its CPU and a stale acknowledgement cannot complete the retry.
    #[test]
    fn tlb_all_targets_exact_generation_and_missing_timeout() {
        let fake = Fake::new();
        fake.expected_sends.set(2);
        let publication = Cell::new(0);
        call_many_inner(
            &fake,
            &[1, 2],
            IpiReason::TlbShootdown,
            Duration::from_millis(10),
            |generation| publication.set(generation),
        )
        .unwrap();
        assert_eq!(publication.get(), 1);
        assert!(fake.mailboxes[1].tlb.is_acknowledged(1));
        assert!(fake.mailboxes[2].tlb.is_acknowledged(1));
        fake.sent.borrow_mut().clear();
        fake.missing.set(Some(2));
        let error = call_many_inner(
            &fake,
            &[1, 2],
            IpiReason::TlbShootdown,
            Duration::from_millis(12),
            |generation| assert_eq!(generation, 2),
        )
        .unwrap_err();
        let IpiError::Timeout(details) = error else {
            panic!("wrong failure: {error:?}")
        };
        assert_eq!(details.operation, IpiReason::TlbShootdown);
        assert_eq!(details.logical_cpu, 2);
        assert_eq!(details.physical_cpu, Some(12));
        assert_eq!(details.generation, Some(2));
        assert_eq!(details.acknowledged, 1);
        assert!(details.observed_at >= details.deadline);
        fake.mailboxes[2].tlb.acknowledge(2);
        assert!(!fake.mailboxes[2].tlb.is_acknowledged(3));
        fake.sent.borrow_mut().clear();
        fake.missing.set(None);
        call_many_inner(
            &fake,
            &[1, 2],
            IpiReason::TlbShootdown,
            Duration::from_millis(100),
            |generation| assert_eq!(generation, 3),
        )
        .unwrap();
        assert!(fake.mailboxes[1].tlb.is_acknowledged(3));
        assert!(fake.mailboxes[2].tlb.is_acknowledged(3));
    }

    /// Clears reservations and asynchronous coalescing after transport rejection.
    ///
    /// A subsequent publication must initiate a new physical send.
    #[test]
    fn rejected_transport_permits_retry() {
        let fake = Fake::new();
        fake.failed_send.set(Some(1));
        assert!(matches!(
            notify_inner(&fake, 1, IpiReason::Reschedule),
            Err(IpiError::Transport(_))
        ));
        fake.failed_send.set(None);
        notify_inner(&fake, 1, IpiReason::Reschedule).unwrap();
        assert_eq!(*fake.sent.borrow(), vec![1, 1]);
        fake.sent.borrow_mut().clear();
        fake.expected_sends.set(1);
        fake.failed_send.set(Some(1));
        assert!(
            call_many_inner(
                &fake,
                &[1],
                IpiReason::Test,
                Duration::from_millis(10),
                |_| {}
            )
            .is_err()
        );
        assert!(fake.mailboxes[1].test.reserve());
        fake.mailboxes[1].test.release();
        fake.sent.borrow_mut().clear();
        fake.failed_send.set(None);
        call_many_inner(
            &fake,
            &[1],
            IpiReason::Test,
            Duration::from_millis(20),
            |_| {},
        )
        .unwrap();
    }

    /// Observes payload writes through the Release pending publication and Acquire drain.
    ///
    /// Independent threads exercise the real atomic mailbox boundary.
    #[test]
    fn publication_orders_payload_and_acknowledgement() {
        use std::sync::Arc;
        let mailbox = Arc::new(Mailbox::new());
        let payload = Arc::new(AtomicU64::new(0));
        let remote_mailbox = Arc::clone(&mailbox);
        let remote_payload = Arc::clone(&payload);
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !remote_mailbox.test.is_acknowledged(17) {
                assert!(std::time::Instant::now() < deadline);
                remote_mailbox.drain(|reason, generation| {
                    assert_eq!(reason, IpiReason::Test);
                    assert_eq!(generation, 17);
                    assert_eq!(remote_payload.load(Ordering::Relaxed), 0xfeed);
                    true
                });
                core::hint::spin_loop();
            }
        });
        assert!(mailbox.test.reserve());
        payload.store(0xfeed, Ordering::Relaxed);
        mailbox.test.publish_generation(17);
        mailbox.publish(IpiReason::Test);
        worker.join().unwrap();
        assert!(mailbox.test.is_acknowledged(17));
    }
    /// Preserves a newer notification when an older transport rejects after another drain.
    ///
    /// Failure cleanup must own its publication rather than blindly clearing the current bit.
    #[test]
    fn failed_sender_cannot_clear_newer_reschedule() {
        let fake = Fake::new();
        fake.failed_send.set(Some(1));
        fake.replace_failed.set(true);
        assert!(matches!(
            notify_inner(&fake, 1, IpiReason::Reschedule),
            Err(IpiError::Transport(_))
        ));
        fake.mailboxes[1].drain(|_, _| true);
        assert_eq!(
            fake.mailboxes[1].reschedule_count.load(Ordering::Acquire),
            2
        );
        assert_eq!(*fake.sent.borrow(), vec![1, 1]);
    }
}
