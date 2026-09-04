//! Cooperative FIFO scheduling.
//!
//! Owns synchronized per-CPU queues, pinned root and idle tasks, and deferred task reclamation.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    hint,
    sync::atomic::{AtomicU64, Ordering},
};

use expercpu::def_percpu;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::VirtAddrRange;

use super::{
    DomainError, DomainPolicy, DomainRef, JoinHandle, Task, TaskId, TaskRef, create_domain,
    run_queue::TaskRunQueue,
    stack::KernelStack,
    timer_queue::TimerQueue,
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
static RUN_QUEUES: LazyInit<Box<[SpinNoIrq<TaskRunQueue>]>> = LazyInit::new();

/// Global cooperative sleeper ownership and deadline state.
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

/// The shared cooperative sleeper queue.
static SLEEP_STATE: SpinNoIrq<SleepState> = SpinNoIrq::new(SleepState::new());

/// Tasks whose timer entries expired in interrupt context and await scheduler publication.
static PENDING_SLEEP_WAKE: SpinNoIrq<Vec<(u64, Arc<Task>)>> = SpinNoIrq::new(Vec::new());

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

/// Whether a newly entered task should enable local interrupts after its handoff.
///
/// Resumed tasks retain their own saved call-site decision on their suspended stack.
#[def_percpu]
static FIRST_ENTRY_IRQ_ENABLED: bool = false;

/// Initializes the first queue topology for all logical CPUs.
fn init_run_queues(cpu_count: usize) {
    if RUN_QUEUES.get().is_some() {
        return;
    }
    let queues = (0..cpu_count)
        .map(|_| SpinNoIrq::new(TaskRunQueue::new()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    RUN_QUEUES.init_once(queues);
}

/// Returns one synchronized queue for a logical CPU.
fn run_queue(cpu_id: usize) -> &'static SpinNoIrq<TaskRunQueue> {
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
    if domain.cpu_snapshot().snapshot().is_empty() {
        domain.suspend(task);
        return;
    }
    let cpu_id = placement_cpu(&task, preferred_cpu);
    run_queue(cpu_id).lock().enqueue(task);
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

/// Selects the next local normal or idle context.
///
/// The caller holds the local run-queue lock and has already moved the outgoing task out of
/// Running.
unsafe fn select_next(queue: &mut TaskRunQueue, cpu_id: usize) -> Next {
    if let Some(task) = queue.dequeue() {
        task.start_running(cpu_id)
            .expect("dequeued task could not become Running");
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

    let target = run_queue(cpu_id)
        .lock()
        .take_target(target.id())
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
/// Currently, the scheduler is just a per-CPU cooperative scheduler.
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
    // Membership changes are committed before task metadata. If the destination update fails,
    // the source remains authoritative. A future lock-free CPU-membership structure may replace
    // this ordered metadata protocol after its memory-ordering proof.
    domain.add_cpu(cpu_id)?;
    if let Err(error) = source.remove_cpu(cpu_id) {
        let _ = domain.remove_cpu(cpu_id);
        return Err(error);
    }
    source.retire_task();
    domain.account_task();
    current.set_domain(domain);
    let previous = CURRENT_DOMAIN.read_current();
    CURRENT_DOMAIN.write_current(Arc::into_raw(Arc::clone(domain)) as usize);
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
        enqueue_task(task, crate::mp::current_cpu_id());
    }
}

/// Returns the earliest cooperative sleep deadline.
///
/// Timer programming uses this value alongside the periodic kernel deadline.
pub(super) fn next_sleep_deadline() -> Option<exarch::time::TimeValue> {
    SLEEP_STATE.lock().timers.next_deadline()
}

/// Wakes cooperative sleepers whose deadlines have arrived.
///
/// Generation validation occurs before run-queue publication so stale timer entries cannot wake a
/// task that has already begun another sleep operation.
pub(super) fn wake_sleepers(now: exarch::time::TimeValue) {
    let sleepers = {
        let mut state = SLEEP_STATE.lock();
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
    PENDING_SLEEP_WAKE.lock().extend(sleepers);
}

/// Publishes timer-expired tasks to the runnable queue outside interrupt context.
fn publish_pending_sleep_wakes() {
    let pending = core::mem::take(&mut *PENDING_SLEEP_WAKE.lock());
    for (generation, task) in pending {
        if task.wake_from_sleep(generation) {
            enqueue_task(task, crate::mp::current_cpu_id());
        }
    }
}

/// Blocks the current task until an absolute timer deadline.
///
/// Past and zero-length requests yield without adding a timer entry. A blocked task retains one
/// scheduler ownership token in [`SLEEP_STATE`] until its matching timer entry wakes it.
pub(super) fn sleep_until(deadline: exarch::time::TimeValue) {
    assert_scheduler_initialized();

    let now = exarch::time::monotonic_time();
    if deadline <= now {
        yield_now();
        return;
    }

    let irq_enabled = exarch::trap::local_enabled();
    exarch::trap::disable_local();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    // SAFETY: Local interrupts remain disabled while cloning the current ownership token.
    let current = unsafe { current_task() };
    let generation = current.begin_sleep();
    current
        .block(cpu_id)
        .expect("current task could not become timer-blocked");
    {
        let mut state = SLEEP_STATE.lock();
        state.timers.push(deadline, current.id(), generation);
        state
            .tasks
            .push((current.id(), generation, Arc::clone(&current)));
    }

    let previous_context = current.context_ptr();
    let mut queue = run_queue(cpu_id).lock();
    // SAFETY: The current task is Blocked, all queue state is locked, and no lock crosses switch.
    let next = unsafe { select_next(&mut queue, cpu_id) };
    drop(queue);
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
    let mut queue = run_queue(cpu_id).lock();
    // SAFETY: The current task is Blocked, all queue state is locked, and no lock crosses switch.
    let next = unsafe { select_next(&mut queue, cpu_id) };
    drop(queue);
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
    publish_pending_sleep_wakes();
    finish_switch();
    let cpu_id = crate::mp::current_cpu_id();
    // SAFETY: Local interrupts remain disabled through current-token inspection and handoff.
    let current = unsafe { current_task() };
    let mut queue = run_queue(cpu_id).lock();
    if queue.is_empty() {
        exarch::trap::enable_local();
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
        queue.enqueue(Arc::clone(&current));
    }
    let previous_context = current.context_ptr();
    // SAFETY: The lock owns every candidate. Outgoing state and queue publication are complete.
    let next = unsafe { select_next(&mut queue, cpu_id) };
    if next.id() == current.id() {
        drop(queue);
        if irq_enabled {
            exarch::trap::enable_local();
        }
        return;
    }
    drop(queue);
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
    let mut queue = run_queue(cpu_id).lock();
    // SAFETY: The exiting task owns this CPU and the scheduler lock owns every candidate.
    let next = unsafe { select_next(&mut queue, cpu_id) };
    drop(queue);
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
