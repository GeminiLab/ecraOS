//! Kernel task related types.

use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kspin::SpinNoIrq;

use super::stack::KernelStack;
use super::wait_queue::{TaskWaitQueue, TaskWaitQueueGuard, validate_join};

/// A stable kernel task identifier.
///
/// Identifiers remain associated with a task for its entire lifetime.
pub type TaskId = u64;

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
    /// A task that has completed and can never run again.
    Exited,
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
            Self::Running { .. } => Err(TaskStateError::InvalidTransition),
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
            Self::Created | Self::Blocked => Err(TaskStateError::InvalidTransition),
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
            Self::Created | Self::Runnable | Self::Blocked => {
                Err(TaskStateError::InvalidTransition)
            }
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
            Self::Created | Self::Runnable | Self::Blocked => {
                Err(TaskStateError::InvalidTransition)
            }
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
            waiters: TaskWaitQueue::new(),
        }
    }

    /// Publishes task completion.
    ///
    /// Release ordering makes all closure effects visible before completion is observed.
    fn publish_completed(&self) {
        self.completed.store(true, Ordering::Release);
    }

    /// Returns whether the task completion has been published.
    ///
    /// The acquire load observes closure effects before a joiner returns.
    pub(super) fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
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
    /// Whether the context is ready for publication or saving at a switch boundary.
    context_initialized: AtomicBool,
    /// The scheduler placement policy for this task.
    placement: TaskPlacement,
    /// The monotonically increasing generation of the current sleep request.
    sleep_generation: AtomicU64,
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
            context_initialized: AtomicBool::new(false),
            sleep_generation: AtomicU64::new(0),
            placement,
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
            context_initialized: AtomicBool::new(true),
            sleep_generation: AtomicU64::new(0),
            placement: TaskPlacement::Pinned(cpu_id),
        })
    }

    /// Returns the task's stable identifier.
    ///
    /// The value never changes during the task lifetime.
    pub const fn id(&self) -> TaskId {
        self.id
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
            TaskState::Created | TaskState::Runnable | TaskState::Blocked => {
                Err(TaskStateError::InvalidTransition)
            }
        }
    }

    /// Returns a snapshot of the lifecycle state.
    ///
    /// The snapshot is used by ownership checks and scheduler transitions.
    pub fn state(&self) -> TaskState {
        *self.state.lock()
    }

    /// Creates a join handle for this task's completion state.
    ///
    /// The returned handle does not retain the task stack after task exit.
    pub(crate) fn join_handle(&self) -> JoinHandle {
        JoinHandle {
            task_id: self.id,
            completion: Arc::clone(&self.completion),
            joined: false,
        }
    }

    /// Publishes completion and transitions the task to Exited on its owning CPU.
    ///
    /// The returned waiters must be routed to runnable queues after this task's completion lock is
    /// no longer held.
    pub(super) fn complete(&self, cpu_id: usize) -> alloc::vec::Vec<Arc<Task>> {
        self.completion.publish_completed();
        self.state
            .lock()
            .exit(cpu_id)
            .expect("task completed outside its owning CPU");
        self.completion.take_waiters()
    }

    /// Marks a blocked task runnable after one completion wakeup.
    ///
    /// Exactly one waiter generation may wake this task.
    pub(crate) fn wake_from_completion(&self) -> Result<(), TaskStateError> {
        self.state.lock().make_runnable()
    }

    /// Blocks this task on its owning CPU.
    ///
    /// The scheduler calls this only while the task is Running and the completion queue is locked.
    pub(crate) fn block(&self, cpu_id: usize) -> Result<(), TaskStateError> {
        self.state.lock().block(cpu_id)
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

    /// Wakes a task if the timer entry still names its current sleep request.
    ///
    /// Stale entries are discarded without changing a newer sleep or another blocked state.
    pub(crate) fn wake_from_sleep(&self, generation: u64) -> bool {
        if self.sleep_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let result = self.state.lock().make_runnable();
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
    /// Whether this handle has consumed its join operation.
    joined: bool,
}

impl JoinHandle {
    /// Waits cooperatively until the target task exits.
    ///
    /// Joining the current task is rejected because it cannot make progress while blocked.
    pub fn join(mut self) {
        let current_task_id = super::scheduler::current_task_id();
        assert_eq!(
            validate_join(current_task_id, self.task_id),
            Ok(()),
            "task attempted to join itself"
        );
        while !self.completion.is_completed() {
            if current_task_id.is_some() {
                super::scheduler::block_current_on(&self.completion);
            } else {
                super::scheduler::yield_now();
            }
        }
        self.joined = true;
    }

    /// Returns whether the target task has exited.
    ///
    /// This query never blocks or changes scheduler state.
    #[expect(unused)]
    pub fn is_finished(&self) -> bool {
        self.completion.is_completed()
    }
}
