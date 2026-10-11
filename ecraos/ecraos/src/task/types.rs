//! Kernel task related types.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use kspin::SpinNoIrq;

use super::TaskError;
use super::wait_queue::{TaskWaitQueue, TaskWaitQueueGuard, validate_join};
use super::{
    domain::{DomainId, DomainRef, SchedulingDomain},
    stack::KernelStack,
};

/// A stable kernel task identifier.
///
/// Identifiers remain associated with a task for its entire lifetime.
pub type TaskId = u64;

/// A shared reference to one task.
pub type TaskRef = Arc<Task>;

/// A non-owning reference to one task.
pub type WeakTaskRef = Weak<Task>;

/// Kernel task lifecycle states.
///
/// Runnable represents ownership by the future run queue, while Running records the sole CPU that
/// currently owns execution of the task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    /// A task that has not yet been published as runnable.
    Created,
    /// A task that is eligible to run and may be enqueued once.
    Runnable,
    /// A task executing on exactly one CPU.
    Running {
        /// The logical identifier of the CPU executing the task.
        cpu_id: usize,
    },
    /// A task waiting for its current wait generation to be woken.
    Blocked,
    /// A task sleeping until its local timer deadline.
    Sleeping,
    /// A task registered on one wait queue and one timer deadline.
    TimedWaiting,
    /// A task that has completed and can never run again.
    Exited,
}

/// The published outcome of a task that reached `Exited`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskExitStatus {
    /// The task body returned normally.
    Completed,
}

/// The outcome of one timed wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitResult {
    /// The wait queue notified the task before its deadline.
    Woken,
    /// The wait deadline expired before notification.
    TimedOut,
}

/// Errors returned by rejected task lifecycle transitions.
///
/// Each error names the invariant that prevented the requested transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskStateError {
    /// The task is already runnable and publishing it again would double enqueue it.
    AlreadyRunnable,
    /// The task is already executing and cannot be started on another CPU.
    AlreadyRunning,
    /// The requested CPU does not own the running task.
    WrongCpu,
    /// The task has exited and cannot transition again.
    Exited,
    /// The requested transition is invalid from the current lifecycle state.
    InvalidTransition,
}

/// A scheduler placement policy for one task.
///
/// Tasks with any-CPU placement may be published on the current CPU's queue, while pinned tasks
/// must remain on their designated logical CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskPlacement {
    /// Allows publication on the scheduler-selected CPU.
    AnyCpu,
    /// Restricts publication and execution to one logical CPU.
    Pinned(usize),
}

impl TaskState {
    /// Publishes a created or blocked task as runnable.
    ///
    /// A runnable task rejects another publication so future queue ownership remains unique.
    pub fn make_runnable(&mut self) -> Result<(), TaskStateError> {
        match self {
            Self::Created | Self::Blocked => {
                *self = Self::Runnable;
                Ok(())
            }
            Self::Runnable => Err(TaskStateError::AlreadyRunnable),
            Self::Exited => Err(TaskStateError::Exited),
            Self::Running { .. } | Self::Sleeping | Self::TimedWaiting => {
                Err(TaskStateError::InvalidTransition)
            }
        }
    }

    /// Starts a runnable task on one CPU.
    ///
    /// A running task rejects all additional starts so it cannot execute on two CPUs.
    pub fn start_running(&mut self, cpu_id: usize) -> Result<(), TaskStateError> {
        match self {
            Self::Runnable => {
                *self = Self::Running { cpu_id };
                Ok(())
            }
            Self::Running { .. } => Err(TaskStateError::AlreadyRunning),
            Self::Exited => Err(TaskStateError::Exited),
            Self::Created | Self::Blocked | Self::Sleeping | Self::TimedWaiting => {
                Err(TaskStateError::InvalidTransition)
            }
        }
    }

    /// Blocks a task on its owning CPU.
    ///
    /// Only the CPU recorded by the running state may relinquish execution ownership.
    pub fn block(&mut self, cpu_id: usize) -> Result<(), TaskStateError> {
        match self {
            Self::Running { cpu_id: owner } if *owner == cpu_id => {
                *self = Self::Blocked;
                Ok(())
            }
            Self::Running { .. } => Err(TaskStateError::WrongCpu),
            Self::Exited => Err(TaskStateError::Exited),
            Self::Created
            | Self::Runnable
            | Self::Blocked
            | Self::Sleeping
            | Self::TimedWaiting => Err(TaskStateError::InvalidTransition),
        }
    }

    /// Exits a task on its owning CPU.
    ///
    /// Exited is terminal and only the CPU currently executing the task may enter it.
    pub fn exit(&mut self, cpu_id: usize) -> Result<(), TaskStateError> {
        match self {
            Self::Running { cpu_id: owner } if *owner == cpu_id => {
                *self = Self::Exited;
                Ok(())
            }
            Self::Running { .. } => Err(TaskStateError::WrongCpu),
            Self::Exited => Err(TaskStateError::Exited),
            Self::Created
            | Self::Runnable
            | Self::Blocked
            | Self::Sleeping
            | Self::TimedWaiting => Err(TaskStateError::InvalidTransition),
        }
    }
}

/// Converts one owned [`Arc`] strong reference into a context argument.
///
/// The returned raw argument exclusively represents the consumed strong reference until
/// [`execution_arg_into_arc`] reconstructs it exactly once.
pub(crate) fn execution_arg_from_arc<T>(owner: Arc<T>) -> usize {
    Arc::into_raw(owner) as usize
}

/// Reconstructs the owned [`Arc`] strong reference represented by a context argument.
///
/// This consumes the raw token without changing its strong count.
///
/// # Safety
///
/// `argument` must have been returned by one unmatched [`execution_arg_from_arc`] call for `T`.
/// It must be reconstructed exactly once.
pub(crate) unsafe fn execution_arg_into_arc<T>(argument: usize) -> Arc<T> {
    // SAFETY: The caller guarantees that this pointer is one unmatched Arc::into_raw token for T.
    unsafe { Arc::from_raw(argument as *const T) }
}

/// Narrow task completion publication state.
pub(super) struct Completion {
    /// Whether the task closure has returned.
    completed: AtomicBool,
    /// Stable encoded exit outcome.
    status: AtomicU8,
    /// The internal wait queue used by joiners.
    waiters: TaskWaitQueue,
}

impl Completion {
    /// Creates unpublished completion state.
    ///
    /// Neither completion nor waiter notification is visible initially.
    const fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
            status: AtomicU8::new(0),
            waiters: TaskWaitQueue::new(),
        }
    }

    /// Publishes task completion.
    ///
    /// Release ordering makes all closure effects visible before completion is observed.
    fn publish_completed(&self) {
        self.status.store(1, Ordering::Relaxed);
        self.completed.store(true, Ordering::Release);
    }

    /// Returns whether the task completion has been published.
    ///
    /// The acquire load observes closure effects before a joiner returns.
    pub(super) fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    /// Returns the published exit outcome, if the task has completed.
    pub(super) fn status(&self) -> Option<TaskExitStatus> {
        if self.is_completed() {
            Some(TaskExitStatus::Completed)
        } else {
            None
        }
    }

    /// Locks the completion waiter queue.
    ///
    /// The caller must release this guard before any scheduler context switch.
    pub(super) fn lock_waiters(&self) -> TaskWaitQueueGuard<'_> {
        self.waiters.lock()
    }

    /// Wakes all completion waiters and returns their task tokens.
    ///
    /// The caller enqueues returned tasks after releasing the completion queue lock.
    pub(super) fn take_waiters(&self) -> alloc::vec::Vec<Arc<Task>> {
        self.waiters.lock().wake_all()
    }
}

/// An owned kernel task.
///
/// The task owns its stable ID, protected lifecycle, architecture context, guarded stack, closure,
/// completion publication and current-stack lifetime marker.
pub struct Task {
    /// The stable identifier assigned at creation.
    id: TaskId,
    /// The lifecycle and exclusive running-CPU ownership.
    state: SpinNoIrq<TaskState>,
    /// The architecture context prepared before publication and written by switch code only at
    /// exclusive incoming or outgoing context boundaries.
    context: UnsafeCell<exarch::context::TaskContext>,
    /// The owned guarded kernel stack.
    stack: KernelStack,
    /// The optional one-shot task body for a fresh task's initial trampoline.
    ///
    /// Adopted root tasks already execute their continuation and therefore leave this field empty.
    body: UnsafeCell<Option<Box<dyn FnOnce() + Send>>>,
    /// The completion state that outlives execution until final task reclamation.
    completion: Arc<Completion>,
    /// Whether execution is still using this task's stack.
    current: AtomicBool,
    /// Whether this task is registered on a synchronization wait queue.
    waiting: AtomicBool,
    /// Raw pointer to the one synchronization wait queue owning this registration.
    wait_queue: AtomicU64,
    /// Result published by the winner of a timed wait wake/timeout race.
    wait_result: AtomicU8,
    /// Whether the context is ready for publication or saving at a switch boundary.
    context_initialized: AtomicBool,
    /// The scheduler placement policy for this task.
    placement: TaskPlacement,
    /// The monotonically increasing generation of the current sleep request.
    sleep_generation: AtomicU64,
    /// The CPU owning the current timer entry, or `usize::MAX` when none is registered.
    timer_cpu: AtomicUsize,
    /// Remaining timer ticks for the current preemptive slice.
    remaining_slice: AtomicU64,
    /// Weak ownership link to the scheduling domain.
    domain: SpinNoIrq<Weak<SchedulingDomain>>,
    /// Stable domain identity retained after the domain is destroyed.
    domain_id: AtomicU64,
}

// SAFETY: Task construction requires a Send closure. All shared mutable state is atomic or
// protected by SpinNoIrq, except the context and body UnsafeCells. The context is accessed uniquely
// at switch boundaries while the task is not running. The body is consumed exactly once by its own
// trampoline.
unsafe impl Send for Task {}

// SAFETY: The same invariants as Send permit shared references. Lifecycle ownership prevents two
// CPUs from running the task, and current-stack reclamation prevents access after stack release.
unsafe impl Sync for Task {}

impl Task {
    /// Creates a detached task whose architecture context is not yet bootstrapped.
    ///
    /// All fallible stack allocation completes before the task exists. A task dropped before
    /// scheduler context preparation has no raw execution token and releases normally.
    pub fn new_detached(
        id: TaskId,
        body: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>, crate::mem::allocs::vmalloc::VMAllocError> {
        Self::new_detached_with_placement(id, body, TaskPlacement::AnyCpu)
    }

    /// Creates a detached task with an explicit scheduler placement policy.
    ///
    /// The task remains private and Created until its context is prepared and it is published.
    pub(super) fn new_detached_with_placement(
        id: TaskId,
        body: impl FnOnce() + Send + 'static,
        placement: TaskPlacement,
    ) -> Result<Arc<Self>, crate::mem::allocs::vmalloc::VMAllocError> {
        let task = Arc::new(Self {
            id,
            state: SpinNoIrq::new(TaskState::Created),
            context: UnsafeCell::new(exarch::context::TaskContext::default()),
            stack: KernelStack::allocate()?,
            body: UnsafeCell::new(Some(Box::new(body))),
            completion: Arc::new(Completion::new()),
            current: AtomicBool::new(false),
            waiting: AtomicBool::new(false),
            wait_queue: AtomicU64::new(0),
            wait_result: AtomicU8::new(0),
            context_initialized: AtomicBool::new(false),
            sleep_generation: AtomicU64::new(0),
            timer_cpu: AtomicUsize::new(usize::MAX),
            remaining_slice: AtomicU64::new(0),
            placement,
            domain: SpinNoIrq::new(Weak::new()),
            domain_id: AtomicU64::new(0),
        });
        Ok(task)
    }

    /// Adopts the current execution continuation as a pinned root task.
    ///
    /// The supplied stack is already active and its context storage is populated by the first
    /// scheduler switch. No trampoline body or fresh context frame is installed.
    pub(super) fn adopt_current(id: TaskId, stack: KernelStack, cpu_id: usize) -> Arc<Self> {
        Arc::new(Self {
            id,
            state: SpinNoIrq::new(TaskState::Running { cpu_id }),
            context: UnsafeCell::new(exarch::context::TaskContext::default()),
            stack,
            body: UnsafeCell::new(None),
            completion: Arc::new(Completion::new()),
            current: AtomicBool::new(true),
            waiting: AtomicBool::new(false),
            wait_queue: AtomicU64::new(0),
            wait_result: AtomicU8::new(0),
            context_initialized: AtomicBool::new(true),
            sleep_generation: AtomicU64::new(0),
            timer_cpu: AtomicUsize::new(usize::MAX),
            remaining_slice: AtomicU64::new(0),
            placement: TaskPlacement::Pinned(cpu_id),
            domain: SpinNoIrq::new(Weak::new()),
            domain_id: AtomicU64::new(0),
        })
    }

    /// Returns the task's stable identifier.
    ///
    /// The value never changes during the task lifetime.
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Returns the task's current scheduling domain, if it remains alive.
    pub fn domain(&self) -> Option<DomainRef> {
        let _guard = kernel_guard::NoPreempt::new();
        self.domain.lock().upgrade()
    }

    /// Returns the stable scheduling-domain identifier.
    pub fn domain_id(&self) -> DomainId {
        self.domain_id.load(Ordering::Acquire)
    }

    /// Associates a newly created task with one scheduling domain.
    pub(super) fn set_domain(&self, domain: &DomainRef) {
        let _guard = kernel_guard::NoPreempt::new();
        *self.domain.lock() = Arc::downgrade(domain);
        self.domain_id.store(domain.id(), Ordering::Release);
        let ticks = domain
            .policy()
            .slice_ticks(core::time::Duration::from_millis(10))
            .unwrap_or(0);
        self.remaining_slice.store(ticks, Ordering::Release);
    }

    /// Accounts one timer tick and reports whether the slice expired.
    pub(super) fn account_tick(&self) -> bool {
        let mut current = self.remaining_slice.load(Ordering::Acquire);
        while current != 0 {
            match self.remaining_slice.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return current == 1,
                Err(observed) => current = observed,
            }
        }
        false
    }

    /// Restores a full slice after a cooperative yield or replacement-free tick.
    pub(super) fn reset_slice(&self) {
        let ticks = self
            .domain()
            .and_then(|domain| {
                domain
                    .policy()
                    .slice_ticks(core::time::Duration::from_millis(10))
            })
            .unwrap_or(0);
        self.remaining_slice.store(ticks, Ordering::Release);
    }

    /// Returns the task's scheduler placement policy.
    pub(super) const fn placement(&self) -> TaskPlacement {
        self.placement
    }

    /// Publishes the task as runnable.
    ///
    /// The protected state machine rejects publication before context preparation and duplicate
    /// publication.
    pub fn make_runnable(&self) -> Result<(), TaskStateError> {
        let _guard = kernel_guard::NoPreempt::new();
        assert!(
            self.context_initialized.load(Ordering::Acquire),
            "task context was not prepared before publication"
        );
        self.state.lock().make_runnable()
    }

    /// Prepares the task's initial architecture context exactly once.
    ///
    /// Callers invoke this while the task is still private and Created, before publishing it as
    /// runnable or making scheduler state active. The context owns one strong reference that the
    /// initial trampoline reconstructs exactly once.
    pub(crate) fn prepare_context(self: &Arc<Self>) {
        let _guard = kernel_guard::NoPreempt::new();
        let state = self.state.lock();
        assert_eq!(
            *state,
            TaskState::Created,
            "task context must be prepared before publication"
        );
        assert!(
            !self.context_initialized.load(Ordering::Acquire),
            "task context initialized twice"
        );
        let argument = execution_arg_from_arc(Arc::clone(self));
        // SAFETY: The lifecycle lock and Created state keep the task private during initialization.
        // The argument owns one strong reference and the initialized context passes it to
        // task_trampoline exactly once. All fallible allocation completed in new_detached before
        // the token was created.
        unsafe {
            exarch::context::init_task_context(
                &mut *self.context.get(),
                self.stack.top(),
                task_trampoline,
                argument,
            )
        };
        self.context_initialized.store(true, Ordering::Release);
    }

    /// Marks the task running on one CPU.
    ///
    /// The prepared-context assertion, state transition, and current-stack marker are published
    /// before switching to the task.
    pub fn start_running(self: &Arc<Self>, cpu_id: usize) -> Result<(), TaskStateError> {
        assert!(
            self.context_initialized.load(Ordering::Acquire),
            "task context was not prepared before starting"
        );
        let mut state = self.state.lock();
        state.start_running(cpu_id)?;
        self.current.store(true, Ordering::Release);
        Ok(())
    }

    /// Returns a running task to the runnable state on its owning CPU.
    ///
    /// The current-stack marker remains set until a different stack runs the post-switch hook.
    pub(crate) fn yield_runnable(&self, cpu_id: usize) -> Result<(), TaskStateError> {
        let mut state = self.state.lock();
        match *state {
            TaskState::Running { cpu_id: owner } if owner == cpu_id => {
                *state = TaskState::Runnable;
                Ok(())
            }
            TaskState::Running { .. } => Err(TaskStateError::WrongCpu),
            TaskState::Exited => Err(TaskStateError::Exited),
            TaskState::Created
            | TaskState::Runnable
            | TaskState::Blocked
            | TaskState::Sleeping
            | TaskState::TimedWaiting => Err(TaskStateError::InvalidTransition),
        }
    }

    /// Returns a snapshot of the lifecycle state.
    ///
    /// The snapshot is used by ownership checks and scheduler transitions.
    pub fn state(&self) -> TaskState {
        *self.state.lock()
    }

    /// Waits cooperatively for this task to reach `Exited`.
    pub fn join(&self) -> Result<(), TaskError> {
        let mut handle = JoinHandle {
            task_id: self.id,
            completion: Arc::clone(&self.completion),
            task: Weak::new(),
            joined: false,
        };
        handle.join()
    }

    /// Creates a join handle for this task's completion state.
    ///
    /// The returned handle does not retain the task stack after task exit.
    pub(crate) fn join_handle(self: &Arc<Self>) -> JoinHandle {
        JoinHandle {
            task_id: self.id,
            completion: Arc::clone(&self.completion),
            task: Arc::downgrade(self),
            joined: false,
        }
    }

    /// Publishes completion and transitions the task to Exited on its owning CPU.
    ///
    /// The returned waiters must be routed to runnable queues after this task's completion lock is
    /// no longer held.
    pub(super) fn complete(&self, cpu_id: usize) -> alloc::vec::Vec<Arc<Task>> {
        self.state
            .lock()
            .exit(cpu_id)
            .expect("task completed outside its owning CPU");
        if let Some(domain) = self.domain() {
            domain.retire_task();
        }
        self.completion.publish_completed();
        self.completion.take_waiters()
    }

    /// Marks a blocked task runnable after one completion wakeup.
    ///
    /// Exactly one waiter generation may wake this task.
    pub(crate) fn wake_from_completion(&self) -> Result<(), TaskStateError> {
        let _guard = kernel_guard::NoPreempt::new();
        self.state.lock().make_runnable()
    }

    /// Wakes one synchronization waiter and publishes the notification result.
    pub(crate) fn wake_from_wait(&self) -> Result<(), TaskStateError> {
        let _guard = kernel_guard::NoPreempt::new();
        let mut state = self.state.lock();
        match *state {
            TaskState::Blocked => state.make_runnable(),
            TaskState::TimedWaiting => {
                *state = TaskState::Runnable;
                self.wait_result.store(1, Ordering::Release);
                self.timer_cpu.store(usize::MAX, Ordering::Release);
                Ok(())
            }
            _ => Err(TaskStateError::InvalidTransition),
        }
    }

    /// Times out one synchronization waiter if its generation is still current.
    pub(crate) fn timeout_wait(&self, generation: u64) -> bool {
        if self.sleep_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let _guard = kernel_guard::NoPreempt::new();
        let mut state = self.state.lock();
        if *state != TaskState::TimedWaiting {
            return false;
        }
        *state = TaskState::Runnable;
        self.wait_result.store(2, Ordering::Release);
        self.timer_cpu.store(usize::MAX, Ordering::Release);
        true
    }

    /// Rolls back a timed wait that could not acquire timer ownership.
    pub(crate) fn cancel_timed_wait(&self, generation: u64, cpu_id: usize) -> bool {
        if self.sleep_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let mut state = self.state.lock();
        if *state != TaskState::TimedWaiting {
            return false;
        }
        *state = TaskState::Running { cpu_id };
        self.timer_cpu.store(usize::MAX, Ordering::Release);
        self.wait_result.store(0, Ordering::Release);
        true
    }

    /// Blocks this task on its owning CPU.
    ///
    /// The scheduler calls this only while the task is Running and the completion queue is locked.
    pub(crate) fn block(&self, cpu_id: usize) -> Result<(), TaskStateError> {
        let _guard = kernel_guard::NoPreempt::new();
        self.state.lock().block(cpu_id)
    }

    /// Moves a running task into timer-owned sleeping state.
    pub(crate) fn sleep(&self, cpu_id: usize) -> Result<(), TaskStateError> {
        let _guard = kernel_guard::NoPreempt::new();
        let mut state = self.state.lock();
        match *state {
            TaskState::Running { cpu_id: owner } if owner == cpu_id => {
                *state = TaskState::Sleeping;
                self.timer_cpu.store(cpu_id, Ordering::Release);
                Ok(())
            }
            TaskState::Running { .. } => Err(TaskStateError::WrongCpu),
            TaskState::Exited => Err(TaskStateError::Exited),
            _ => Err(TaskStateError::InvalidTransition),
        }
    }

    /// Starts one uniquely identified sleep request.
    ///
    /// A new generation invalidates every timer entry left by an earlier request.
    pub(crate) fn begin_sleep(&self) -> u64 {
        self.sleep_generation
            .fetch_add(1, Ordering::AcqRel)
            .checked_add(1)
            .expect("task sleep generation space exhausted")
    }

    /// Returns the generation of the current timer operation.
    pub(crate) fn sleep_generation(&self) -> u64 {
        self.sleep_generation.load(Ordering::Acquire)
    }

    /// Wakes a task if the timer entry still names its current sleep request.
    ///
    /// Stale entries are discarded without changing a newer sleep or another blocked state.
    pub(crate) fn wake_from_sleep(&self, generation: u64) -> bool {
        if self.sleep_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let _guard = kernel_guard::NoPreempt::new();
        let result = {
            let mut state = self.state.lock();
            match *state {
                TaskState::Sleeping => {
                    *state = TaskState::Runnable;
                    self.timer_cpu.store(usize::MAX, Ordering::Release);
                    Ok(())
                }
                _ => state.make_runnable(),
            }
        };
        result.is_ok()
    }

    /// Returns the architecture context pointer used by switch assembly.
    ///
    /// The caller must uphold the architecture context's unique-access and running-state rules.
    pub(crate) const fn context_ptr(&self) -> *mut exarch::context::TaskContext {
        self.context.get()
    }

    /// Returns whether any CPU may still be executing on this task's stack.
    ///
    /// Scheduler selection uses this during the unlocked context-switch handoff window.
    pub(crate) fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire)
    }

    /// Marks the stack non-current after execution has moved elsewhere.
    ///
    /// Deferred reaping must call this only from another active stack.
    pub(crate) fn mark_not_current(&self) {
        self.current.store(false, Ordering::Release);
    }

    /// Claims this task's single wait-queue registration slot.
    pub(crate) fn claim_waiting(&self) -> Result<(), TaskError> {
        self.waiting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| TaskError::AlreadyWaiting)
    }

    /// Releases this task's wait-queue registration slot.
    pub(crate) fn release_waiting(&self) {
        self.waiting.store(false, Ordering::Release);
    }

    /// Associates this task with its synchronization wait queue.
    pub(crate) fn set_wait_queue(&self, queue: *const ()) {
        self.wait_queue
            .store(queue as usize as u64, Ordering::Release);
    }

    /// Removes and returns the synchronization wait queue pointer.
    pub(crate) fn take_wait_queue(&self) -> Option<*const ()> {
        let value = self.wait_queue.swap(0, Ordering::AcqRel);
        (value != 0).then_some(value as usize as *const ())
    }

    /// Begins one timed wait and returns its generation token.
    pub(crate) fn begin_timed_wait(&self, cpu_id: usize) -> Result<u64, TaskStateError> {
        let generation = self.begin_sleep();
        let _guard = kernel_guard::NoPreempt::new();
        let mut state = self.state.lock();
        match *state {
            TaskState::Running { cpu_id: owner } if owner == cpu_id => {
                *state = TaskState::TimedWaiting;
                self.wait_result.store(0, Ordering::Release);
                self.timer_cpu.store(cpu_id, Ordering::Release);
                Ok(generation)
            }
            TaskState::Running { .. } => Err(TaskStateError::WrongCpu),
            _ => Err(TaskStateError::InvalidTransition),
        }
    }

    /// Returns the timer CPU for the current timed wait, if one is registered.
    pub(crate) fn timer_cpu(&self) -> Option<usize> {
        let cpu = self.timer_cpu.load(Ordering::Acquire);
        (cpu != usize::MAX).then_some(cpu)
    }

    /// Returns and clears the result of the most recent timed wait.
    pub(crate) fn take_wait_result(&self) -> Option<WaitResult> {
        match self.wait_result.swap(0, Ordering::AcqRel) {
            1 => Some(WaitResult::Woken),
            2 => Some(WaitResult::TimedOut),
            _ => None,
        }
    }
}

impl Drop for Task {
    /// Verifies current-stack exclusion before releasing owned fields.
    ///
    /// KernelStack drops only after this assertion proves execution moved to another stack.
    fn drop(&mut self) {
        assert!(
            !self.current.load(Ordering::Acquire),
            "attempted to reclaim the current task stack"
        );
    }
}

/// Runs a task's owned closure and transfers control to the narrow exit callback.
///
/// The raw argument owns one Arc strong reference. Completion is published before terminal state
/// and waiter notification are observed, and this function never returns.
unsafe extern "C" fn task_trampoline(task: usize) -> ! {
    // SAFETY: prepare_context supplies exactly one unmatched Arc::into_raw token to the initial
    // context, and architecture entry invokes this trampoline exactly once with that token.
    let task: Arc<Task> = unsafe { execution_arg_into_arc(task) };
    super::scheduler::finish_initial_switch();
    let body = unsafe { (&mut *task.body.get()).take() }.expect("task body already consumed");
    body();
    let waiters = task.complete(crate::mp::current_cpu_id());
    super::scheduler::wake_tasks(waiters);
    super::exit_current(task)
}

/// A handle that joins one cooperative task completion.
///
/// The handle owns completion state but does not keep an exited task's stack alive.
pub struct JoinHandle {
    /// Stable identity used to reject self-join.
    task_id: TaskId,
    /// Completion state shared with the task and all observers.
    completion: Arc<Completion>,
    /// Weak target used for cross-domain wake placement without retaining the task.
    task: Weak<Task>,
    /// Whether this handle has consumed its join operation.
    joined: bool,
}

impl JoinHandle {
    /// Waits cooperatively until the target task exits.
    ///
    /// Joining the current task is rejected because it cannot make progress while blocked.
    pub fn join(&mut self) -> Result<(), TaskError> {
        if self.joined {
            return Err(TaskError::AlreadyJoined);
        }
        let current_task_id = super::scheduler::current_task_id();
        if validate_join(current_task_id, self.task_id).is_err() {
            return Err(TaskError::WouldDeadlock);
        }
        while !self.completion.is_completed() {
            if current_task_id.is_some() {
                super::scheduler::block_current_on(&self.completion);
            } else {
                super::scheduler::yield_now();
            }
        }
        self.joined = true;
        Ok(())
    }

    /// Returns whether the target task has exited.
    ///
    /// This query never blocks or changes scheduler state.
    pub fn is_finished(&self) -> bool {
        self.completion.is_completed()
    }

    /// Returns the completed task outcome without consuming the join handle.
    pub fn status(&self) -> Option<TaskExitStatus> {
        self.completion.status()
    }
}
