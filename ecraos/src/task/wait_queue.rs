//! Cooperative completion wait queues.
//!
//! The pure model defines the publication and block-commit protocol shared by host tests and the
//! kernel completion queue. Architecture-specific switching is layered on top by the scheduler.

use alloc::{collections::VecDeque, vec::Vec};

/// Errors returned before a join waiter is published.
///
/// Self-join is rejected because the current task cannot complete while waiting on itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinValidationError {
    /// The target task is the caller itself.
    SelfJoin,
}

/// Validates that a task may join a target task.
///
/// A caller without a scheduler identity may join any target.
pub fn validate_join(
    current_task_id: Option<u64>,
    target_task_id: u64,
) -> Result<(), JoinValidationError> {
    if current_task_id == Some(target_task_id) {
        Err(JoinValidationError::SelfJoin)
    } else {
        Ok(())
    }
}

use alloc::sync::Arc;
use kspin::{SpinNoIrq, SpinNoIrqGuard};

use super::{Task, TaskId};

/// Errors returned by the kernel completion waiter queue.
///
/// Duplicate task membership would permit one task to be woken more than once for one generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskWaitQueueError {
    /// The task already occupies a completion waiter position.
    Duplicate,
}

/// The state of one kernel completion waiter.
///
/// Published entries close the wake-before-block-commit race.
#[derive(Debug)]
enum TaskWaiterState {
    /// Published but not yet committed as blocked.
    Published,
    /// Committed as blocked and awaiting completion wakeup.
    Blocked,
}

/// One kernel completion waiter and its task ownership token.
///
/// The queue owns the token until wakeup or cancellation removes the entry.
struct TaskWaiter {
    /// The task waiting for completion.
    task: Arc<Task>,
    /// The publication state used by the block protocol.
    state: TaskWaiterState,
}

/// The internal completion wait queue.
///
/// This queue is intentionally limited to task completion and `join`. Its lock is acquired before
/// the scheduler run-queue lock and is released before any architecture context switch.
pub(super) struct TaskWaitQueue {
    /// Waiter entries protected from CPUs and local interrupt handlers.
    inner: SpinNoIrq<VecDeque<TaskWaiter>>,
}

impl TaskWaitQueue {
    /// Creates an empty completion wait queue.
    ///
    /// No task initially waits for the completion event.
    pub(super) const fn new() -> Self {
        Self {
            inner: SpinNoIrq::new(VecDeque::new()),
        }
    }

    /// Locks the completion wait queue.
    ///
    /// The guard disables local interrupts and must be dropped before switching contexts.
    pub(super) fn lock(&self) -> TaskWaitQueueGuard<'_> {
        TaskWaitQueueGuard {
            guard: self.inner.lock(),
        }
    }
}

/// A locked completion wait queue.
///
/// The guard owns the lock across waiter publication and block commit to close the wakeup race.
pub(super) struct TaskWaitQueueGuard<'a> {
    /// The IRQ-safe queue lock guard.
    guard: SpinNoIrqGuard<'a, VecDeque<TaskWaiter>>,
}

impl TaskWaitQueueGuard<'_> {
    /// Publishes one task as a completion waiter.
    ///
    /// The caller must hold this guard while checking completion and committing the block.
    pub(super) fn register(&mut self, task: Arc<Task>) -> Result<(), TaskWaitQueueError> {
        if self
            .guard
            .iter()
            .any(|waiter| waiter.task.id() == task.id())
        {
            return Err(TaskWaitQueueError::Duplicate);
        }
        self.guard.push_back(TaskWaiter {
            task,
            state: TaskWaiterState::Published,
        });
        Ok(())
    }

    /// Commits one published task as blocked unless completion already woke it.
    ///
    /// `false` means the waiter was consumed by a wake-before-commit race and the caller must not
    /// block.
    pub(super) fn commit_block(&mut self, task_id: TaskId) -> bool {
        let Some(waiter) = self
            .guard
            .iter_mut()
            .find(|waiter| waiter.task.id() == task_id)
        else {
            return false;
        };
        waiter.state = TaskWaiterState::Blocked;
        true
    }

    /// Removes and returns all published and blocked completion waiters.
    ///
    /// The caller releases this guard before enqueuing the returned tasks.
    pub(super) fn wake_all(&mut self) -> Vec<Arc<Task>> {
        self.guard.drain(..).map(|waiter| waiter.task).collect()
    }
}
