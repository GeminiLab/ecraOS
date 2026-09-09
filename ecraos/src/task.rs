//! Scheduling-domain kernel task runtime.
//!
//! Connects task ownership, synchronized FIFO scheduling, and cooperative blocking.

use alloc::sync::Arc;
use memory_addr::VirtAddrRange;

mod domain;
pub(crate) mod preempt;
mod queue;
mod run_queue;
mod scheduler;
mod stack;
mod sync;
mod timer_queue;
mod trace;
mod types;
mod wait_queue;

pub use domain::{
    CpuSet, DomainError, DomainId, DomainLifecycle, DomainPolicy, DomainRef, SchedulingDomain,
    create_domain, lookup_domain,
};
pub use preempt::{RescheduleGuard, can_schedule_now, request_reschedule};
pub use queue::{PhysicalQueue, QueueState, QueueStateError};
pub use scheduler::TargetError;
pub use sync::{Condvar, Mutex, MutexGuard, Semaphore, WaitQueue};
pub use trace::{TraceEvent, TraceEventKind, TraceRing};
pub use types::TaskStateError;
pub use types::{
    JoinHandle, Task, TaskExitStatus, TaskId, TaskRef, TaskState, WaitResult, WeakTaskRef,
};

/// Unified task and synchronization operation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskError {
    /// The caller is not an ordinary task.
    NotTaskContext,
    /// The caller is inside a critical section.
    InCriticalSection,
    /// The requested operation cannot block.
    CannotBlock,
    /// The task already occupies a wait queue.
    AlreadyWaiting,
    /// The operation would wait for the caller itself.
    WouldDeadlock,
    /// A one-shot join handle was already consumed.
    AlreadyJoined,
    /// A timed operation reached its deadline.
    TimedOut,
    /// Required scheduler storage could not be allocated.
    AllocationFailed,
    /// A target task or domain is invalid.
    InvalidTarget,
    /// Outstanding ownership prevents the operation.
    Busy,
    /// The domain is draining or destroying.
    DomainDestroying,
    /// The domain is permanently destroyed.
    DomainDestroyed,
}

/// Initializes the scheduler for the current CPU.
///
/// This function should and should only be called once per CPU during its initialization. The
/// current stack is adopted as a pinned root task.
///
/// Scheduling domains may be cooperative or timer-preemptive. Timer interrupts only force a
/// switch for tasks in preemptive domains, while cooperative domains consume pending requests at
/// explicit safe points.
pub fn init_scheduler_current_cpu(current_stack: VirtAddrRange) {
    scheduler::init_current_cpu(current_stack)
}

/// Spawns one detached kernel task.
pub fn spawn(body: impl FnOnce() + Send + 'static) -> JoinHandle {
    scheduler::spawn(body)
}

/// Switches the running task and current CPU to another scheduling domain.
pub fn switch_current_to(domain: &DomainRef) -> Result<(), DomainError> {
    scheduler::switch_current_to(domain)
}

/// Returns the current kernel task identity when called from an active scheduler.
pub fn current_task_id() -> Option<TaskId> {
    scheduler::current_task_id()
}

/// Returns the current ordinary task reference, if called from task context.
pub fn current_task() -> Option<TaskRef> {
    scheduler::current_task_ref()
}

/// Accounts one local timer tick before the interrupt-exit scheduling safe point.
pub fn timer_tick() {
    preempt::timer_tick();
}

/// Handles the task scheduler portion of a local timer interrupt exit.
///
/// Timer wake publication precedes this call. Preemptive domains consume a pending request here
/// only when `kernel_guard` reports that the interrupted task was outside a critical section.
pub fn timer_interrupt_exit() {
    scheduler::timer_interrupt_exit();
}

/// Cooperatively yields the current execution context.
///
/// No timer or interrupt-exit path invokes this function during Stage 2A.
pub fn yield_now() {
    scheduler::yield_now()
}

/// Yields cooperatively to one locally runnable task.
pub fn yield_to(target: Arc<Task>) -> Result<(), TargetError> {
    scheduler::yield_to(target)
}

/// Exits the current task.
fn exit_current(task: Arc<Task>) -> ! {
    scheduler::exit_current(task)
}

/// Exits the current task and switches permanently to one locally runnable task.
pub fn exit_and_yield_to(target: Arc<Task>) -> ! {
    scheduler::exit_and_yield_to(target)
}

/// Sleeps the current task for a relative duration.
///
/// A zero duration yields immediately. Positive durations block cooperatively until the monotonic
/// deadline is delivered by the kernel timer.
pub fn sleep(duration: exarch::time::Duration) -> Result<(), TaskError> {
    sleep_until(exarch::time::monotonic_time().saturating_add(duration))
}

/// Sleeps the current task until an absolute monotonic deadline.
///
/// A past deadline yields immediately and never enters the sleeper queue.
pub fn sleep_until(deadline: exarch::time::TimeValue) -> Result<(), TaskError> {
    scheduler::sleep_until(deadline)
}

/// Wakes timer-blocked tasks whose deadlines have arrived.
///
/// The timer interrupt calls this while local interrupt delivery is serialized on the current CPU.
pub fn wake_sleepers(now: exarch::time::TimeValue) {
    scheduler::wake_sleepers(now)
}

/// Returns the earliest timer-blocked task deadline.
///
/// The timer interrupt uses this to program a one-shot deadline.
pub fn next_sleep_deadline() -> Option<exarch::time::TimeValue> {
    scheduler::next_sleep_deadline()
}

/// Runs the current CPU's cooperative idle loop.
///
/// Secondary CPUs enter this after completing their existing startup initialization.
pub fn run_idle() -> ! {
    scheduler::enter_idle()
}
