//! Cooperative FIFO scheduling.
//!
//! Owns synchronized per-CPU queues, pinned root and idle tasks, and deferred task reclamation.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    hint,
    sync::atomic::{AtomicU64, Ordering},
};

use expercpu::def_percpu;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::VirtAddrRange;

use super::{
    DomainError, DomainPolicy, DomainRef, JoinHandle, PhysicalQueue, QueueState, Task, TaskId,
    TaskRef, create_domain,
    domain::MAX_CPU_NUM,
    stack::KernelStack,
    timer_queue::TimerQueue,
    trace::{TraceEventKind, TraceRing},
    types::{Completion, TaskPlacement},
};

/// The first dynamically assigned scheduler task identifier.
///
/// Stable identifiers are unique across normal and idle tasks.
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// The synchronized per-CPU cooperative FIFO queues.
///
/// The scheduler accesses this storage through placement helpers so a future global or work-stealing
/// backend can replace the topology without changing task handoff APIs.
static RUN_QUEUES: LazyInit<Box<[SpinNoIrq<PhysicalQueue<TaskRef>>]>> = LazyInit::new();

/// Per-CPU sleeper ownership and deadline state.
///
/// The lock is held only while publishing or removing sleep records, never across a context
/// switch or task wakeup enqueue.
struct SleepState {
    /// Ordered timer entries used to find expired sleepers.
    timers: TimerQueue,
    /// Arc ownership retained while each task is blocked on its timer.
    tasks: Vec<(TaskId, u64, Arc<Task>)>,
}

impl SleepState {
    /// Creates an empty sleeper state.
    const fn new() -> Self {
        Self {
            timers: TimerQueue::new(),
            tasks: Vec::new(),
        }
    }
}

/// The local timer queue and its strong sleeping-task ownership.
///
/// Each CPU owns this state, so a timer interrupt never serializes unrelated CPUs. A sleeping task
/// remains on the CPU that registered its deadline in Stage 2B. CPU offline support and a
/// lock-free timer data structure are intentionally deferred until their ownership and
/// memory-ordering proof exists.
#[def_percpu]
static SLEEP_STATE: SpinNoIrq<SleepState> = SpinNoIrq::new(SleepState::new());

/// The current task's owned `Arc` token on this CPU.
///
/// Only the local CPU accesses this token with interrupts disabled.
#[def_percpu]
static CURRENT_TASK: usize = 0;

/// The current CPU's strong scheduling-domain token.
#[def_percpu]
static CURRENT_DOMAIN: usize = 0;

/// This CPU's permanent idle-task `Arc` token.
///
/// The token remains owned until shutdown and is cloned only with local interrupts disabled.
#[def_percpu]
static IDLE_TASK: usize = 0;

/// The outgoing task token awaiting post-switch reclamation.
///
/// The incoming stack consumes this token before its next scheduling operation.
#[def_percpu]
static DEFERRED_REAP: usize = 0;

/// Whether this CPU has valid root, idle, and ownership state.
///
/// Publication occurs only after all other per-CPU scheduler fields are initialized.
#[def_percpu]
static SCHEDULER_ACTIVE: bool = false;

/// The cooperative runtime's reserved preemption nesting count.
///
/// Stage 2A leaves this at zero and performs no interrupt-exit preemption.
#[def_percpu]
static PREEMPT_COUNT: usize = 0;

/// The cooperative runtime's reserved local reschedule indication.
///
/// Stage 2A does not set this from timers or remote CPUs.
#[def_percpu]
static NEED_RESCHEDULE: bool = false;

/// Per-CPU bounded scheduler trace storage.
///
/// Scheduler writers use this ring without allocation or locking because each CPU records only
/// its own events. Readers must obtain a local snapshot at a controlled diagnostic point.
#[def_percpu]
static TRACE: TraceRing<64> = TraceRing::new();

/// The next remote CPU offset inspected when this CPU is idle.
///
/// This cursor is local-only, resets when a CPU changes scheduling domain, and supports one
/// bounded same-domain tail steal. Stage 2B intentionally has no proactive load balancer.
#[def_percpu]
static STEAL_CURSOR: usize = 0;

/// Whether a newly entered task should enable local interrupts after its handoff.
///
/// Resumed tasks retain their own saved call-site decision on their suspended stack.
#[def_percpu]
static FIRST_ENTRY_IRQ_ENABLED: bool = false;

/// Records one local scheduler event without allocation or blocking.
fn record_trace(kind: TraceEventKind, task: &Task) {
    // SAFETY: Scheduling paths record only to the current CPU's private ring. No lock is needed,
    // and this must remain allocation-free for interrupt and handoff paths.
    unsafe {
        TRACE
            .current_ref_mut_raw()
            .push(kind, task.id(), task.domain_id());
    }
}

/// Returns the local CPU's timer ownership queue.
fn sleep_state() -> &'static SpinNoIrq<SleepState> {
    // SAFETY: All callers execute in the current CPU context. The returned storage is private to
    // that CPU and is synchronized by SpinNoIrq for task and timer-interrupt access.
    unsafe { SLEEP_STATE.current_ref_raw() }
}

/// Initializes the first queue topology for all logical CPUs.
fn init_run_queues(cpu_count: usize) {
    if RUN_QUEUES.get().is_some() {
        return;
    }
    assert!(
        cpu_count <= MAX_CPU_NUM,
        "Stage 2B supports at most {} logical CPUs",
        MAX_CPU_NUM
    );
    let queues = (0..cpu_count)
        .map(|_| SpinNoIrq::new(PhysicalQueue::new()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    RUN_QUEUES.init_once(queues);
}

/// Returns one synchronized queue for a logical CPU.
fn run_queue(cpu_id: usize) -> &'static SpinNoIrq<PhysicalQueue<TaskRef>> {
    RUN_QUEUES
        .get()
        .expect("run queues must be initialized")
        .get(cpu_id)
        .expect("invalid run queue CPU")
}

/// Returns the queue CPU selected by one task's placement policy.
fn placement_cpu(task: &Task, preferred_cpu: usize) -> usize {
    if let Some(domain) = task.domain() {
        let members = domain.cpu_snapshot();
        if members.contains(preferred_cpu) {
            return preferred_cpu;
        }
        if let Some(cpu) = domain.select_cpu() {
            return cpu;
        }
    }
    match task.placement() {
        TaskPlacement::AnyCpu => preferred_cpu,
        TaskPlacement::Pinned(cpu_id) => cpu_id,
    }
}

/// Enqueues one task on its placement-selected queue.
fn enqueue_task(task: Arc<Task>, preferred_cpu: usize) {
    // Queue publication follows NoPreemptIrqSave -> domain metadata -> Queue locks. This guard is
    // intentionally short-lived and is dropped before any context switch. A future lock-free
    // queue could replace this lock after its memory-ordering proof.
    let _guard = kernel_guard::NoPreemptIrqSave::new();
    let Some(domain) = task.domain() else {
        return;
    };
    let domain_id = domain.id();
    let members = domain.cpu_snapshot().snapshot();
    if members.is_empty() {
        domain.suspend(task);
        return;
    }
    let cpu_id = placement_cpu(&task, preferred_cpu);
    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
    // lifecycle. The membership snapshot above is deliberately released before this queue lock;
    // a future lock-free queue and membership structure may replace the current locks after a
    // complete memory-ordering proof.
    let mut queue = run_queue(cpu_id).lock();
    if queue.state() == QueueState::Active(domain_id) {
        let task_id = task.id();
        queue
            .push(domain_id, task)
            .expect("validated active queue rejected task publication");
        // This producer is running on the current CPU, which owns the corresponding trace ring.
        // A future lock-free trace transport may replace the current local ring after a complete
        // memory-ordering proof.
        // SAFETY: enqueue_task executes in the current CPU context with local preemption held.
        unsafe {
            TRACE
                .current_ref_mut_raw()
                .push(TraceEventKind::Enqueue, task_id, domain_id);
        }
        super::preempt::request_reschedule();
    } else {
        drop(queue);
        domain.suspend(task);
    }
}

/// A selected owned scheduler destination.
type Next = Arc<Task>;

/// Allocates a stable scheduler task identifier.
///
/// Exhaustion is treated as a fatal kernel invariant because identifiers must never repeat.
fn allocate_task_id() -> TaskId {
    NEXT_TASK_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("kernel task identifier space exhausted")
}

/// Clones an owned `Arc` token stored as a raw per-CPU pointer.
///
/// The caller must disable local interrupts so the per-CPU owner cannot be replaced concurrently.
unsafe fn clone_token(raw: usize) -> Arc<Task> {
    assert_ne!(raw, 0, "attempted to clone an empty task token");
    let pointer = raw as *const Task;
    // SAFETY: The raw value is a live Arc::into_raw token protected by local interrupt exclusion.
    unsafe { Arc::increment_strong_count(pointer) };
    // SAFETY: The preceding increment created exactly one owned strong reference for this result.
    unsafe { Arc::from_raw(pointer) }
}

/// Returns whether the scheduler is initialized on this CPU.
pub fn initialized() -> bool {
    SCHEDULER_ACTIVE.read_current()
}

/// Asserts that the scheduler is initialized on this CPU.
fn assert_scheduler_initialized() {
    assert!(initialized(), "scheduler is not initialized");
}

/// Asserts that the scheduler is not initialized on this CPU.
fn assert_scheduler_not_initialized() {
    assert!(!initialized(), "scheduler is already initialized");
}

/// Returns a clone of the current task.
///
/// The caller must disable local interrupts across this read and subsequent ownership transition.
unsafe fn current_task() -> Arc<Task> {
    let raw = CURRENT_TASK.read_current();
    unsafe { clone_token(raw) }
}

/// Returns a clone of the current CPU's scheduling domain.
unsafe fn current_domain() -> DomainRef {
    let raw = CURRENT_DOMAIN.read_current();
    assert_ne!(raw, 0, "current CPU has no scheduling domain");
    let pointer = raw as *const super::SchedulingDomain;
    // SAFETY: The per-CPU token is a live Arc protected by local interrupt exclusion.
    unsafe { Arc::increment_strong_count(pointer) };
    // SAFETY: The preceding increment created exactly one owned Arc for this result.
    unsafe { Arc::from_raw(pointer) }
}

/// Returns the current owned task identifier.
///
/// Local interrupts are disabled only for the raw per-CPU ownership read and restored afterward.
pub(super) fn current_task_id() -> Option<TaskId> {
    if !initialized() {
        return None;
    }

    let _guard = kernel_guard::NoPreempt::new();
    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    // SAFETY: Local interrupts are disabled while cloning the per-CPU Arc token.
    let id = unsafe { current_task().id() };
    if irq_enabled {
        exarch::trap::enable_local();
    }
    Some(id)
}

/// Returns the logical CPU executing the current task.
pub(super) fn current_cpu_id() -> usize {
    crate::mp::current_cpu_id()
}

/// Returns the current ordinary task reference, excluding the idle task.
pub(super) fn current_task_ref() -> Option<TaskRef> {
    if !initialized() {
        return None;
    }
    let task = unsafe { current_task() };
    let idle = unsafe { idle_task() };
    (task.id() != idle.id()).then_some(task)
}

/// Returns a clone of this CPU's idle task.
///
/// The caller must disable local interrupts while accessing the per-CPU token.
unsafe fn idle_task() -> Arc<Task> {
    unsafe { clone_token(IDLE_TASK.read_current()) }
}

/// Selects the idle task and validates its lifecycle transition.
///
/// The local run-queue lock is held by the caller and local interrupts remain disabled.
unsafe fn select_idle(cpu_id: usize) -> Next {
    let idle = unsafe { idle_task() };
    idle.start_running(cpu_id)
        .expect("idle task could not become Running");
    idle
}

/// Pulls at most one tail task from another active queue in the same domain.
///
/// The snapshot is intentionally released before any Queue lock. Each source is revalidated
/// while locked because a concurrent local domain switch may have made the snapshot stale.
fn idle_pull(domain: &DomainRef, cpu_id: usize) -> Option<TaskRef> {
    let sources = domain
        .cpu_snapshot()
        .snapshot()
        .into_iter()
        .filter(|candidate| *candidate != cpu_id)
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return None;
    }

    let cursor = STEAL_CURSOR.read_current();
    STEAL_CURSOR.write_current(cursor.wrapping_add(1));
    for offset in 0..sources.len() {
        let source_cpu = sources[(cursor + offset) % sources.len()];
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle. This path holds only the source Queue lock, so it never reverses a pair-lock
        // order. A future lock-free work-stealing deque may replace this lock after a complete
        // memory-ordering proof.
        let mut source = run_queue(source_cpu).lock();
        // The CPU snapshot above is intentionally stale once this Queue lock is acquired. Queue
        // attachment is the authoritative recheck here, so this path never reacquires the domain
        // metadata lock beneath a Queue lock. The global order remains NoPreemptIrqSave -> domain
        // metadata -> Queue by CpuId -> task lifecycle. A future lock-free membership structure
        // could provide an atomic generation check after its memory-ordering proof.
        if source.state() != QueueState::Active(domain.id()) {
            continue;
        }
        if let Some(task) = source
            .steal_tail(domain.id())
            .expect("validated idle-pull source detached from its domain")
        {
            return Some(task);
        }
    }
    None
}

/// Selects the next local, same-domain stolen, or idle context.
///
/// The outgoing task has already left `Running`. Local FIFO selection precedes one bounded idle
/// pull, and all Queue locks are released before the caller can switch architecture contexts.
unsafe fn select_next(domain: &DomainRef, cpu_id: usize) -> Next {
    let local = {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle. The domain reference is stable and no metadata lock is retained here. A
        // future lock-free local queue may replace this lock after a complete memory-ordering
        // proof.
        let mut queue = run_queue(cpu_id).lock();
        queue
            .pop(domain.id())
            .expect("current CPU queue is not attached to its current domain")
    };
    if let Some(task) = local.or_else(|| idle_pull(domain, cpu_id)) {
        task.start_running(cpu_id)
            .expect("selected task could not become Running");
        record_trace(TraceEventKind::Switch, &task);
        return task;
    }
    unsafe { select_idle(cpu_id) }
}

/// Installs next/current ownership and transfers the outgoing owner to deferred reaping.
///
/// The caller runs on the outgoing stack with local interrupts disabled and no scheduler lock held.
fn install_handoff(next: Next) -> (*const exarch::context::TaskContext, usize) {
    let context = next.context_ptr() as *const _;
    let next_context = context;
    let next_raw = Arc::into_raw(next) as usize;
    let outgoing = CURRENT_TASK.read_current();
    assert_eq!(
        DEFERRED_REAP.read_current(),
        0,
        "previous scheduler handoff was not reaped"
    );
    CURRENT_TASK.write_current(next_raw);
    DEFERRED_REAP.write_current(outgoing);
    (next_context, outgoing)
}

/// Finishes one context-switch ownership handoff on the incoming stack.
///
/// This clears the outgoing stack's current marker before releasing its current-task reference.
fn finish_switch() {
    let outgoing = DEFERRED_REAP.read_current();
    if outgoing == 0 {
        return;
    }
    DEFERRED_REAP.write_current(0);
    // SAFETY: install_handoff placed one unmatched Arc::into_raw token in this per-CPU slot. The
    // incoming stack owns the slot and consumes the token exactly once.
    let outgoing = unsafe { Arc::from_raw(outgoing as *const Task) };
    outgoing.mark_not_current();
    drop(outgoing);
}

/// Errors returned when a deterministic target cannot be claimed locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetError {
    /// The requested target is already the current task.
    SelfTarget,
    /// The target is not Runnable or is still in a switch handoff window.
    NotRunnable,
    /// The target is not present in this CPU's claimable scheduler state.
    NotLocal,
}

/// Claims one target task for immediate execution on the current CPU.
///
/// Idle is a per-CPU task outside the normal queue. Every other target must be removed exactly once
/// from the current CPU's queue before its state changes to Running.
fn claim_target(target: Arc<Task>, cpu_id: usize) -> Result<Arc<Task>, TargetError> {
    let current_id = unsafe { current_task() }.id();
    if target.id() == current_id {
        return Err(TargetError::SelfTarget);
    }

    let idle = unsafe { idle_task() };
    if target.id() == idle.id() {
        if target.state() != super::TaskState::Runnable || target.is_current() {
            return Err(TargetError::NotRunnable);
        }
        target
            .start_running(cpu_id)
            .map_err(|_| TargetError::NotRunnable)?;
        return Ok(target);
    }

    let domain = unsafe { current_domain() };
    let target = run_queue(cpu_id)
        .lock()
        .take_matching(domain.id(), |candidate| candidate.id() == target.id())
        .expect("current CPU queue is not attached to its current domain")
        .ok_or(TargetError::NotLocal)?;
    target
        .start_running(cpu_id)
        .map_err(|_| TargetError::NotRunnable)?;
    Ok(target)
}

/// Switches cooperatively to one locally runnable task and returns when resumed.
pub(super) fn yield_to(target: Arc<Task>) -> Result<(), TargetError> {
    assert_scheduler_initialized();

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let _ = super::preempt::take_reschedule_request();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    let next = match claim_target(target, cpu_id) {
        Ok(next) => next,
        Err(error) => {
            if irq_enabled {
                exarch::trap::enable_local();
            }
            return Err(error);
        }
    };

    current
        .yield_runnable(cpu_id)
        .expect("current task could not yield from Running");
    let idle = unsafe { idle_task() };
    if current.id() != idle.id() {
        enqueue_task(Arc::clone(&current), cpu_id);
    }
    let previous_context = current.context_ptr();
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: The target is Running, the current task is Runnable, all locks are released, and
    // interrupts remain disabled across the architecture handoff.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
    Ok(())
}

/// Exits the current task and switches permanently to one locally runnable target.
pub(super) fn exit_and_yield_to(target: Arc<Task>) -> ! {
    assert_scheduler_initialized();

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let _ = super::preempt::take_reschedule_request();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    let next = claim_target(target, cpu_id)
        .unwrap_or_else(|error| panic!("terminal task target could not be claimed: {error:?}"));
    let waiters = current.complete(cpu_id);
    wake_tasks(waiters);
    let previous_context = current.context_ptr();
    drop(current);
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, outgoing) = install_handoff(next);
    assert_ne!(outgoing, 0, "exiting task has no current ownership token");
    // SAFETY: The current raw token keeps the exited task and stack live until the incoming stack
    // consumes DEFERRED_REAP. The target is Running and all locks are released.
    unsafe { exarch::context::switch(previous_context, next_context) };
    unreachable!("exited task resumed after terminal handoff")
}

/// Initializes the scheduler for the current CPU.
///
/// This function should and should only be called once per CPU during its initialization. The
/// current stack is adopted as a pinned root task.
///
/// The scheduler supports cooperative and timer-driven preemptive domains.
pub(super) fn init_current_cpu(current_stack: VirtAddrRange) {
    assert_scheduler_not_initialized();

    // Initialize the run queues for all logical CPUs.
    //
    // TODO: make it BSP-only.
    init_run_queues(crate::mp::cpu_count());

    // Allocate and initialize the idle task.
    let cpu_id = crate::mp::current_cpu_id();
    let root_domain = create_domain(DomainPolicy::Cooperative)
        .expect("failed to create the initial cooperative scheduling domain");
    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
    // lifecycle. The bootstrap queue has no entries, so attaching it before publishing CPU
    // membership is atomic from normal placement's perspective. A future lock-free queue and
    // membership structure may replace these locks after a complete memory-ordering proof.
    run_queue(cpu_id)
        .lock()
        .activate(root_domain.id())
        .expect("bootstrap physical queue was not inactive");
    root_domain
        .add_cpu(cpu_id)
        .expect("failed to assign the bootstrap CPU to its domain");
    let idle = Task::new_detached_with_placement(
        allocate_task_id(),
        idle_body,
        TaskPlacement::Pinned(cpu_id),
    )
    .expect("failed to allocate idle task stack");
    idle.prepare_context();
    idle.make_runnable()
        .expect("failed to initialize idle task state");

    // Adopt the current stack as a pinned root task.
    let mut root = Task::adopt_current(
        allocate_task_id(),
        KernelStack::adopt_existing(current_stack),
        cpu_id,
    );
    Arc::get_mut(&mut root)
        .expect("root task unexpectedly has another owner")
        .set_domain(&root_domain);
    root_domain.account_task();

    CURRENT_TASK.write_current(Arc::into_raw(root) as usize);
    CURRENT_DOMAIN.write_current(Arc::into_raw(root_domain) as usize);
    IDLE_TASK.write_current(Arc::into_raw(idle) as usize);
    DEFERRED_REAP.write_current(0);
    PREEMPT_COUNT.write_current(0);
    NEED_RESCHEDULE.write_current(false);
    FIRST_ENTRY_IRQ_ENABLED.write_current(false);
    SCHEDULER_ACTIVE.write_current(true);
}

/// Spawns one detached kernel task.
pub(super) fn spawn(body: impl FnOnce() + Send + 'static) -> JoinHandle {
    assert_scheduler_initialized();

    let domain = unsafe { current_domain() };
    let mut task =
        Task::new_detached(allocate_task_id(), body).expect("failed to create kernel task");
    Arc::get_mut(&mut task)
        .expect("new task unexpectedly has another owner")
        .set_domain(&domain);
    let handle = task.join_handle();
    task.prepare_context();
    task.make_runnable()
        .expect("the new kernel task could not become Runnable");

    domain.account_task();
    enqueue_task(task, crate::mp::current_cpu_id());

    handle
}

/// Switches the running task and its CPU to another active scheduling domain.
pub(super) fn switch_current_to(domain: &DomainRef) -> Result<(), DomainError> {
    assert_scheduler_initialized();
    // Lock order is NoPreemptIrqSave -> domain metadata by DomainId -> Queue locks by CpuId ->
    // task lifecycle. This transaction drops every queue lock before changing task metadata and
    // never crosses exarch::context::switch. A future lock-free queue and membership structure
    // may replace these locks after a complete memory-ordering proof.
    let _guard = kernel_guard::NoPreemptIrqSave::new();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    let idle = unsafe { idle_task() };
    if current.id() == idle.id() || !matches!(current.state(), super::TaskState::Running { .. }) {
        return Err(DomainError::InvalidTarget);
    }
    let source = current.domain().ok_or(DomainError::DomainDestroyed)?;
    if source.id() == domain.id() {
        return match domain.lifecycle() {
            super::DomainLifecycle::Active => Ok(()),
            super::DomainLifecycle::Destroyed => Err(DomainError::DomainDestroyed),
            super::DomainLifecycle::Draining | super::DomainLifecycle::Destroying => {
                Err(DomainError::DomainDestroying)
            }
        };
    }
    if domain.lifecycle() != super::DomainLifecycle::Active {
        return Err(if domain.lifecycle() == super::DomainLifecycle::Destroyed {
            DomainError::DomainDestroyed
        } else {
            DomainError::DomainDestroying
        });
    }
    let source_id = source.id();
    let target_id = domain.id();
    let destinations = source
        .cpu_snapshot()
        .snapshot()
        .into_iter()
        .filter(|&candidate| candidate != cpu_id)
        .collect::<Vec<_>>();
    let source_len = {
        let queue = run_queue(cpu_id).lock();
        if queue.state() != QueueState::Active(source_id) {
            return Err(DomainError::InvalidTarget);
        }
        queue.len()
    };

    // Pre-reserve every possible old-domain destination before marking the local queue Draining.
    // A reservation failure leaves membership and queue attachment untouched, which makes the
    // switch transaction rollback-free at its only allocation boundary.
    for &destination_cpu in &destinations {
        let mut queue = run_queue(destination_cpu).lock();
        if queue.state() != QueueState::Active(source_id) || queue.try_reserve(source_len).is_err()
        {
            return Err(DomainError::Busy);
        }
    }

    let entries = {
        let mut queue = run_queue(cpu_id).lock();
        if queue.state() != QueueState::Active(source_id) {
            return Err(DomainError::InvalidTarget);
        }
        queue
            .begin_drain(source_id)
            .map_err(|_| DomainError::Busy)?;
        let entries = queue.drain_fifo(source_id).map_err(|_| DomainError::Busy)?;
        queue
            .finish_drain(source_id)
            .map_err(|_| DomainError::Busy)?;
        queue.activate(target_id).map_err(|_| DomainError::Busy)?;
        entries
    };

    // Claim target membership only after the target queue is attached. The deferred form does not
    // drain suspended ownership, so a rejected add leaves the queue and both domains reversible.
    if let Err(error) = domain.add_cpu_deferred(cpu_id) {
        let mut queue = run_queue(cpu_id).lock();
        queue
            .begin_drain(target_id)
            .expect("failed target queue rollback did not remain Active");
        let target_entries = queue
            .drain_fifo(target_id)
            .expect("failed target queue rollback could not drain");
        assert!(
            target_entries.is_empty(),
            "target queue published tasks before membership transaction committed"
        );
        queue
            .finish_drain(target_id)
            .expect("failed target queue rollback did not finish draining");
        queue
            .activate(source_id)
            .expect("failed target queue rollback could not restore source");
        queue
            .append_fifo(source_id, entries)
            .expect("failed target queue rollback could not restore FIFO entries");
        return Err(error);
    }
    if let Err(error) = source.remove_cpu(cpu_id) {
        let mut queue = run_queue(cpu_id).lock();
        queue
            .begin_drain(target_id)
            .expect("failed source membership rollback did not remain Active");
        let target_entries = queue
            .drain_fifo(target_id)
            .expect("failed source membership rollback could not drain");
        assert!(
            target_entries.is_empty(),
            "target queue published tasks before source removal committed"
        );
        queue
            .finish_drain(target_id)
            .expect("failed source membership rollback did not finish draining");
        queue
            .activate(source_id)
            .expect("failed source membership rollback could not restore source");
        queue
            .append_fifo(source_id, entries)
            .expect("failed source membership rollback could not restore FIFO entries");
        let rollback = domain.remove_cpu(cpu_id);
        assert!(
            rollback.is_ok(),
            "target membership rollback failed: {rollback:?}"
        );
        return Err(error);
    }

    // The detached old-domain queue contents remain FIFO for each CpuId-ordered destination. A
    // destination that changed attachment after the snapshot cannot receive a stale task, so its
    // entry falls back to the source domain's suspended runnable queue.
    let mut suspended = VecDeque::new();
    for (index, task) in entries.into_iter().enumerate() {
        let Some(&destination_cpu) = destinations.get(index % destinations.len().max(1)) else {
            suspended.push_back(task);
            continue;
        };
        let mut queue = run_queue(destination_cpu).lock();
        if queue.state() == QueueState::Active(source_id) {
            queue
                .push(source_id, task)
                .expect("validated source destination rejected FIFO transfer");
        } else {
            suspended.push_back(task);
        }
    }
    source.suspend_fifo(suspended);

    // Queue attachment and both membership changes have committed. The source's Draining lifecycle
    // still permits this removal, which lets explicit destruction make progress after Busy.
    drain_suspended(domain, cpu_id);
    source.retire_task();
    domain.account_task();
    current.set_domain(domain);
    let previous = CURRENT_DOMAIN.read_current();
    CURRENT_DOMAIN.write_current(Arc::into_raw(Arc::clone(domain)) as usize);
    STEAL_CURSOR.write_current(0);
    // SAFETY: `previous` is the strong Arc token held by the old current-domain slot.
    unsafe { drop(Arc::from_raw(previous as *const super::SchedulingDomain)) };
    Ok(())
}

/// Spawns one task into an explicitly selected scheduling domain.
///
/// The caller selects queue placement only after the new task is fully prepared. A domain without
/// an active CPU keeps the task in its suspended FIFO queue until CPU membership is established.
pub(super) fn spawn_in_domain(
    domain: &DomainRef,
    body: impl FnOnce() + Send + 'static,
) -> Result<TaskRef, DomainError> {
    assert_scheduler_initialized();
    let mut task = Task::new_detached(allocate_task_id(), body).map_err(|_| DomainError::Busy)?;
    Arc::get_mut(&mut task)
        .expect("new task unexpectedly has another owner")
        .set_domain(domain);
    task.prepare_context();
    task.make_runnable()
        .expect("new domain task could not become Runnable");
    domain.account_task();
    let result = Arc::clone(&task);
    match domain.select_cpu() {
        Some(cpu) => enqueue_task(task, cpu),
        None => domain.suspend(task),
    }
    Ok(result)
}

/// Publishes suspended domain tasks to a newly active CPU.
pub(super) fn drain_suspended(domain: &super::SchedulingDomain, cpu_id: usize) {
    if !domain.cpu_snapshot().contains(cpu_id) {
        return;
    }
    // Queue activation follows the global order NoPreemptIrqSave -> domain metadata by
    // DomainId -> Queue by CpuId -> task lifecycle. A future lock-free queue state structure
    // could replace this lock after its memory-ordering proof. The queue may have been retired
    // by a previous domain owner, so make the new ownership visible before publishing tasks.
    let queue = run_queue(cpu_id);
    {
        let mut queue = queue.lock();
        match queue.state() {
            QueueState::Inactive => queue
                .activate(domain.id())
                .expect("inactive queue could not be activated for its domain"),
            QueueState::Active(owner) if owner == domain.id() => {}
            state => panic!(
                "CPU {cpu_id} queue is not available for domain {}: {state:?}",
                domain.id()
            ),
        }
    }
    for task in domain.take_suspended() {
        enqueue_task(task, cpu_id);
    }
}

/// Wakes completion waiters and publishes each task to its placement-selected FIFO queue once.
///
/// The completion wait-queue lock is already released when this function is called.
pub(super) fn wake_tasks(tasks: alloc::vec::Vec<Arc<Task>>) {
    for task in tasks {
        task.wake_from_completion()
            .expect("completion waiter was not Blocked");
        if !task.is_current() {
            enqueue_task(task, crate::mp::current_cpu_id());
        }
    }
}

/// Registers a timed wait on the current CPU's timer ownership queue.
pub(super) fn register_timed_wait(
    task: TaskRef,
    generation: u64,
    deadline: exarch::time::TimeValue,
    cpu_id: usize,
) -> Result<(), super::TaskError> {
    if cpu_id >= crate::mp::cpu_count() {
        return Err(super::TaskError::InvalidTarget);
    }
    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task lifecycle
    // -> timer ownership. The timer lock is released before any wake publication or switch. A
    // future lock-free timer structure may replace this lock after a memory-ordering proof.
    let mut state = unsafe { SLEEP_STATE.remote_ref_mut_raw(cpu_id) }.lock();
    state
        .tasks
        .try_reserve(1)
        .map_err(|_| super::TaskError::AllocationFailed)?;
    state
        .timers
        .try_push(deadline, task.id(), generation)
        .map_err(|_| super::TaskError::AllocationFailed)?;
    state.tasks.push((task.id(), generation, task));
    Ok(())
}

/// Cancels one timer-owned wait entry and releases its scheduler ownership.
pub(super) fn cancel_timed_wait(task: &Task, generation: u64, cpu_id: usize) {
    if cpu_id >= crate::mp::cpu_count() {
        return;
    }
    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task lifecycle
    // -> timer ownership. The timer lock is released before any scheduler wake or switch. A
    // future lock-free timer structure may replace this lock after its memory-ordering proof.
    let mut state = unsafe { SLEEP_STATE.remote_ref_mut_raw(cpu_id) }.lock();
    state.timers.cancel(task.id(), generation);
    if let Some(index) = state
        .tasks
        .iter()
        .position(|(task_id, entry_generation, _)| {
            *task_id == task.id() && *entry_generation == generation
        })
    {
        state.tasks.swap_remove(index);
    }
}

/// Makes one generic synchronization waiter runnable in its current scheduling domain.
///
/// Wake paths never context-switch directly. Queue publication completes before the local pending
/// bit is set by [`enqueue_task`], leaving a remote CPU to observe it at its next timer interrupt
/// until Stage 4 adds IPIs.
pub(super) fn wake_waiter(task: TaskRef) {
    if task.wake_from_wait().is_err() {
        return;
    }
    // A remote notifier can win after registration but before the blocked task switches away.
    // Its current-stack marker prevents a duplicate queue publication in that window.
    if !task.is_current() {
        enqueue_task(task, crate::mp::current_cpu_id());
    }
}

/// Blocks the current task after atomically registering it with a generic FIFO wait queue.
///
/// The wait queue performs registration and the `Running -> Blocked` transition under its lock.
/// That lock is released before queue selection and before the architecture context switch.
pub(super) fn block_current_on_wait_queue(
    waiters: &super::sync::WaitQueue,
) -> Result<(), super::TaskError> {
    assert_scheduler_initialized();
    if super::preempt::depth() != 0 {
        return Err(super::TaskError::InCriticalSection);
    }
    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let _ = super::preempt::take_reschedule_request();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    let idle = unsafe { idle_task() };
    if current.id() == idle.id() {
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Err(super::TaskError::NotTaskContext);
    }
    if let Err(error) = waiters.prepare_block(Arc::clone(&current), cpu_id) {
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Err(error);
    }
    record_trace(TraceEventKind::Block, &current);

    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
    // lifecycle -> completion or wait-queue locks. The wait queue is released before this local
    // queue lock and before the switch. A future lock-free waiting structure may replace these
    // locks after a complete memory-ordering proof.
    let domain = unsafe { current_domain() };
    let previous_context = current.context_ptr();
    // SAFETY: The current task is Blocked and no wait-queue lock remains held across selection.
    let next = unsafe { select_next(&domain, cpu_id) };
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: Both contexts remain owned and all scheduler, task, and wait queue locks are gone.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
    Ok(())
}

/// Switches away from a task already registered and blocked on a wait queue.
pub(super) fn block_current_registered() -> Result<(), super::TaskError> {
    assert_scheduler_initialized();
    if super::preempt::depth() != 0 {
        return Err(super::TaskError::InCriticalSection);
    }
    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let _ = super::preempt::take_reschedule_request();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    if !matches!(
        current.state(),
        super::TaskState::Blocked | super::TaskState::TimedWaiting
    ) {
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Ok(());
    }
    record_trace(TraceEventKind::Block, &current);
    let previous_context = current.context_ptr();
    let domain = unsafe { current_domain() };
    // SAFETY: Registration already committed `Blocked`; every Queue lock is released before the
    // architecture handoff. A future lock-free scheduler queue may replace this lock after proof.
    let next = unsafe { select_next(&domain, cpu_id) };
    if next.id() == current.id() {
        current
            .start_running(cpu_id)
            .expect("blocked task selected itself without a wake transition");
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Ok(());
    }
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: No scheduler, task, or wait-queue lock remains held across the switch.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
    Ok(())
}

/// Publishes timer wakeups and then reaches the interrupt-exit scheduling safe point.
///
/// Per-CPU timer ownership is drained directly from the delivering timer interrupt so a CPU-bound
/// preemptive task cannot defer a due wake until it next yields.
pub(super) fn timer_interrupt_exit() {
    schedule_from_timer_interrupt();
}

/// Consumes a local timer-interrupt scheduling request at the interrupt-exit safe point.
///
/// A preemptive task moves to the local queue tail only when another task is available. The
/// outgoing timer-handler frame is saved in its task context and resumes through normal trap
/// return when that task is selected again.
pub(super) fn schedule_from_timer_interrupt() {
    if !super::preempt::can_preempt_from_interrupt() || !super::preempt::take_reschedule_request() {
        return;
    }

    // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
    // lifecycle. No queue or task lifecycle lock survives the context handoff. A future lock-free
    // scheduler queue may replace these locks after a complete memory-ordering proof.
    let _guard = kernel_guard::NoPreemptIrqSave::new();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    let current = unsafe { current_task() };
    let domain = unsafe { current_domain() };
    if !matches!(domain.policy(), DomainPolicy::Preemptive { .. }) {
        // A cooperative domain observes the same request at yield, block, exit, or idle instead
        // of permitting a clock interrupt to force a switch.
        super::preempt::request_reschedule();
        return;
    }

    let mut queue = run_queue(cpu_id).lock();
    if queue.state() != QueueState::Active(domain.id()) || queue.is_empty() {
        current.reset_slice();
        return;
    }
    current
        .yield_runnable(cpu_id)
        .expect("timer preemption could not return current task to Runnable");
    current.reset_slice();
    record_trace(TraceEventKind::ReschedulePending, &current);
    let idle = unsafe { idle_task() };
    if current.id() != idle.id() {
        queue
            .push(domain.id(), Arc::clone(&current))
            .expect("timer preemption detached the local queue");
    }
    drop(queue);
    // SAFETY: The active queue owns all candidates and the current task is already Runnable.
    let next = unsafe { select_next(&domain, cpu_id) };
    if next.id() == current.id() {
        return;
    }
    let previous_context = current.context_ptr();
    FIRST_ENTRY_IRQ_ENABLED.write_current(false);
    let (next_context, _) = install_handoff(next);
    drop(_guard);
    // SAFETY: Interrupts remain disabled, all scheduler locks were released, and saving this
    // timer handler call frame lets the task later return through the architecture trap epilogue.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
}

/// Returns the earliest cooperative sleep deadline.
///
/// Timer programming uses this value alongside the periodic kernel deadline.
pub(super) fn next_sleep_deadline() -> Option<exarch::time::TimeValue> {
    sleep_state().lock().timers.next_deadline()
}

/// Wakes cooperative sleepers whose deadlines have arrived.
///
/// Generation validation occurs before run-queue publication so stale timer entries cannot wake a
/// task that has already begun another sleep operation.
pub(super) fn wake_sleepers(now: exarch::time::TimeValue) {
    let sleepers = {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle -> timer ownership. This timer lock is released before runnable publication.
        // A future lock-free per-CPU timer structure may replace this lock after a complete
        // memory-ordering proof.
        let mut state = sleep_state().lock();
        let entries = state.timers.pop_expired(now, |_, _| true);
        entries
            .into_iter()
            .filter_map(|entry| {
                let index = state.tasks.iter().position(|(task_id, generation, _)| {
                    *task_id == entry.task_id && *generation == entry.generation
                })?;
                Some((entry.generation, state.tasks.swap_remove(index).2))
            })
            .collect::<Vec<_>>()
    };
    for (generation, task) in sleepers {
        if task.state() == super::TaskState::TimedWaiting {
            if let Some(queue) = task.take_wait_queue() {
                // SAFETY: TimedWaiting stores the live WaitQueue address before its timer entry.
                // The synchronization primitive must outlive registered waiters.
                unsafe {
                    (&*(queue as *const super::sync::WaitQueue)).remove_task(task.id());
                }
            }
            task.release_waiting();
            if task.timeout_wait(generation) {
                enqueue_task(task, crate::mp::current_cpu_id());
            }
        } else if task.wake_from_sleep(generation) {
            enqueue_task(task, crate::mp::current_cpu_id());
        }
    }
}

/// Blocks the current task until an absolute timer deadline.
///
/// Past and zero-length requests yield without adding a timer entry. A blocked task retains one
/// scheduler ownership token in the local [`SLEEP_STATE`] until its matching timer entry wakes it.
pub(super) fn sleep_until(deadline: exarch::time::TimeValue) -> Result<(), super::TaskError> {
    assert_scheduler_initialized();

    let now = exarch::time::monotonic_time();
    if deadline <= now {
        yield_now();
        return Ok(());
    }

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    // SAFETY: Local interrupts remain disabled while cloning the current ownership token.
    let current = unsafe { current_task() };
    let generation = current.begin_sleep();
    let registration = (|| -> Result<(), super::TaskError> {
        // Reserve both backing stores before changing the task state. This keeps allocation
        // failure reversible, so a task never remains Sleeping without timer ownership.
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle -> timer ownership. A future lock-free timer structure may replace this lock
        // after its memory-ordering proof.
        let mut state = sleep_state().lock();
        state
            .tasks
            .try_reserve(1)
            .map_err(|_| super::TaskError::AllocationFailed)?;
        state
            .timers
            .try_push(deadline, current.id(), generation)
            .map_err(|_| super::TaskError::AllocationFailed)?;
        Ok(())
    })();
    if let Err(error) = registration {
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Err(error);
    }
    if current.sleep(cpu_id).is_err() {
        cancel_timed_wait(&current, generation, cpu_id);
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return Err(super::TaskError::CannotBlock);
    }
    record_trace(TraceEventKind::Block, &current);
    {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle -> timer ownership. No timer lock crosses task selection or context switch.
        // A future lock-free per-CPU timer structure may replace this lock after a complete
        // memory-ordering proof.
        let mut state = sleep_state().lock();
        state
            .tasks
            .push((current.id(), generation, Arc::clone(&current)));
    }

    let previous_context = current.context_ptr();
    let domain = unsafe { current_domain() };
    // SAFETY: The current task is Blocked, and selection releases every Queue lock before switch.
    let next = unsafe { select_next(&domain, cpu_id) };
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: Both contexts and ownership tokens remain live, interrupts are disabled, and all
    // scheduler locks were released before switching.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
    Ok(())
}

/// Blocks the current task until its completion wait condition changes.
///
/// The completion queue lock covers condition recheck, waiter publication, and block commit. All
/// locks are released before the scheduler selects a destination or crosses the context switch.
pub(super) fn block_current_on(completion: &Completion) {
    assert_scheduler_initialized();

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    // SAFETY: Local interrupts remain disabled while cloning the current ownership token.
    let current = unsafe { current_task() };
    let mut waiters = completion.lock_waiters();
    if completion.is_completed() {
        drop(waiters);
        drop(current);
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return;
    }
    waiters
        .register(Arc::clone(&current))
        .expect("current task already occupies completion wait queue");
    current
        .block(cpu_id)
        .expect("current task could not become Blocked");
    let committed = waiters.commit_block(current.id());
    drop(waiters);
    if !committed {
        current
            .wake_from_completion()
            .expect("woken task could not return to Runnable");
        drop(current);
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return;
    }

    let previous_context = current.context_ptr();
    let domain = unsafe { current_domain() };
    // SAFETY: The current task is Blocked, and selection releases every Queue lock before switch.
    let next = unsafe { select_next(&domain, cpu_id) };
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: Both contexts and ownership tokens remain live, interrupts are disabled, and all
    // scheduler locks were released before switching.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
}

/// Cooperatively yields the current task behind existing runnable work.
///
/// A normal task returns immediately if there is no other local work, while idle polls the local
/// queue without a meaningless self-switch.
pub(super) fn yield_now() {
    assert_scheduler_initialized();

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    // Cooperative domains consume deferred arrivals at an explicit yield safe point. A future
    // lock-free pending bitmap may replace this AcqRel exchange after its memory-ordering proof.
    let _ = super::preempt::take_reschedule_request();
    let cpu_id = crate::mp::current_cpu_id();
    // SAFETY: Local interrupts remain disabled through current-token inspection and handoff.
    let current = unsafe { current_task() };
    let domain = unsafe { current_domain() };
    let mut queue = run_queue(cpu_id).lock();
    if queue.is_empty() {
        drop(queue);
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return;
    }

    current
        .yield_runnable(cpu_id)
        .expect("current task could not yield from Running");
    current.reset_slice();
    // Idle is retained outside the local run queue. All other tasks move behind existing work.
    // SAFETY: The permanent idle token is valid and local interrupts are disabled.
    let idle = unsafe { idle_task() };
    if current.id() != idle.id() {
        queue
            .push(domain.id(), Arc::clone(&current))
            .expect("current CPU queue detached during local yield");
    }
    let previous_context = current.context_ptr();
    drop(queue);
    // SAFETY: Queue publication is complete and select_next releases all Queue locks before handoff.
    let next = unsafe { select_next(&domain, cpu_id) };
    if next.id() == current.id() {
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return;
    }
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    let (next_context, _) = install_handoff(next);
    // SAFETY: Local interrupts are disabled, ownership tokens keep both contexts live, and the
    // scheduler lock was released before crossing the architecture switch boundary.
    unsafe { exarch::context::switch(previous_context, next_context) };
    finish_switch();
    if irq_enabled {
        exarch::trap::enable_local();
    }
}

/// Completes the first context-switch handoff from a newly entered task.
///
/// The architecture trampoline invokes this before running the task body.
pub(super) fn finish_initial_switch() {
    finish_switch();
    if FIRST_ENTRY_IRQ_ENABLED.read_current() {
        exarch::trap::enable_local();
    }
}

/// Schedules away from a terminal task without ever returning to its stack.
///
/// Completion publication and the Exited transition have already occurred in the task trampoline.
pub(super) fn exit_current(task: Arc<Task>) -> ! {
    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    let previous_context = task.context_ptr();
    let domain = unsafe { current_domain() };
    record_trace(TraceEventKind::Exit, &task);
    // SAFETY: The exiting task owns this CPU and selection releases every Queue lock before handoff.
    let next = unsafe { select_next(&domain, cpu_id) };
    FIRST_ENTRY_IRQ_ENABLED.write_current(irq_enabled);
    drop(task);
    let (next_context, outgoing) = install_handoff(next);
    assert_ne!(outgoing, 0, "exiting task has no current ownership token");
    // SAFETY: The current raw token keeps the exited task and context live until the incoming stack
    // consumes deferred ownership. The scheduler lock is released and interrupts are disabled.
    unsafe { exarch::context::switch(previous_context, next_context) };
    unreachable!("exited task resumed after scheduler handoff")
}

/// Exits the current task into the current CPU's permanent idle task.
pub(super) fn enter_idle() -> ! {
    let idle = unsafe { idle_task() };
    exit_and_yield_to(idle)
}

/// Runs the owned idle task body.
///
/// Explicit yields poll the local run queue without adding timer-driven preemption.
fn idle_body() {
    loop {
        yield_now();
        hint::spin_loop();
    }
}
