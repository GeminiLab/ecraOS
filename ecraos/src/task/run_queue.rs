//! Cooperative FIFO run-queue primitives.
//!
//! The shared primitive makes queue invariants testable without kernel allocation or architecture
//! code.

use alloc::collections::{BTreeSet, VecDeque};
use alloc::sync::Arc;

use super::{Task, TaskId, TaskState};

/// Errors returned by FIFO membership publication.
///
/// Duplicate keys would create two queue ownership positions for one logical object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueMembershipError {
    /// The key already occupies a queue position.
    Duplicate,
    /// The value is not eligible to occupy a queue position.
    NotEligible,
}

/// A FIFO queue with duplicate-membership tracking.
///
/// The scheduler stores task ownership tokens in this same primitive that host tests exercise with
/// lightweight values.
#[derive(Debug)]
pub struct FifoQueue<T, K> {
    /// Keyed values ordered from oldest to newest publication.
    entries: VecDeque<(K, T)>,
    /// Stable keys currently represented in `entries`.
    members: BTreeSet<K>,
}

impl<T, K: Copy + Ord> FifoQueue<T, K> {
    /// Creates an empty FIFO queue.
    ///
    /// No keys are initially recorded as members.
    pub const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            members: BTreeSet::new(),
        }
    }

    /// Returns whether no value currently owns a queue position.
    ///
    /// Callers provide any synchronization required around the queue.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Enqueues one eligible value under a unique stable key.
    ///
    /// Ineligible values and duplicate keys are rejected without changing FIFO order.
    pub fn enqueue(
        &mut self,
        key: K,
        value: T,
        eligible: bool,
    ) -> Result<(), QueueMembershipError> {
        if !eligible {
            return Err(QueueMembershipError::NotEligible);
        }
        if !self.members.insert(key) {
            return Err(QueueMembershipError::Duplicate);
        }
        self.entries.push_back((key, value));
        Ok(())
    }

    /// Removes the oldest entry when it satisfies the supplied eligibility check.
    ///
    /// An ineligible head remains queued so FIFO order is preserved. Dequeue releases the value's
    /// membership before returning it.
    pub fn dequeue_if(&mut self, eligible: impl FnOnce(&T) -> bool) -> Option<T> {
        if !eligible(&self.entries.front()?.1) {
            return None;
        }
        let (key, value) = self.entries.pop_front()?;
        assert!(self.members.remove(&key));
        Some(value)
    }

    /// Removes one exact member without changing the order of other entries.
    ///
    /// A missing key leaves both the entries and membership set unchanged.
    pub fn remove(&mut self, key: K) -> Option<T> {
        self.remove_if(key, |_| true)
    }

    /// Removes one exact member when it satisfies the supplied eligibility check.
    ///
    /// An ineligible or missing key leaves both the entries and membership set unchanged.
    pub fn remove_if(&mut self, key: K, eligible: impl FnOnce(&T) -> bool) -> Option<T> {
        let index = self
            .entries
            .iter()
            .position(|(entry_key, _)| *entry_key == key)?;
        if !eligible(&self.entries[index].1) {
            return None;
        }
        let (_, value) = self.entries.remove(index)?;
        assert!(self.members.remove(&key));
        Some(value)
    }
}

/// The kernel run queue protected by its per-CPU scheduler lock.
///
/// Each queued `Arc` is the sole queue-position ownership token for its task.
pub(super) struct TaskRunQueue {
    /// Shared FIFO ordering and membership storage.
    queue: FifoQueue<Arc<Task>, TaskId>,
}

impl TaskRunQueue {
    /// Creates an empty kernel run queue.
    ///
    /// The containing scheduler lock supplies all synchronization.
    pub(super) const fn new() -> Self {
        Self {
            queue: FifoQueue::new(),
        }
    }

    /// Returns whether no task currently owns a queue position.
    ///
    /// Callers hold the per-CPU scheduler lock while observing this value.
    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Enqueues one runnable task exactly once.
    ///
    /// A terminal state or duplicate identifier is a scheduler invariant failure.
    pub(super) fn enqueue(&mut self, task: Arc<Task>) {
        let id = task.id();
        let eligible = task.state() == TaskState::Runnable;
        match self.queue.enqueue(id, task, eligible) {
            Ok(()) => {}
            Err(QueueMembershipError::Duplicate) => {
                panic!("task {id} already occupies the run queue")
            }
            Err(QueueMembershipError::NotEligible) => {
                panic!("task {id} is not runnable")
            }
        }
    }

    /// Removes the oldest task whose former stack is no longer current.
    ///
    /// A task in the unlocked switch handoff remains at the head until the incoming CPU clears its
    /// current-stack marker. This preserves FIFO order without permitting concurrent stack use.
    pub(super) fn dequeue(&mut self) -> Option<Arc<Task>> {
        self.queue.dequeue_if(|task| !task.is_current())
    }

    /// Claims one exact runnable target that is not in a switch handoff window.
    ///
    /// The containing scheduler lock provides synchronization around queue membership and task
    /// state inspection.
    pub(super) fn take_target(&mut self, task_id: TaskId) -> Option<Arc<Task>> {
        self.queue.remove_if(task_id, |task| {
            task.state() == TaskState::Runnable && !task.is_current()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FifoQueue;

    #[test]
    fn removes_exact_member_without_reordering_remaining_entries() {
        let mut queue = FifoQueue::new();
        queue.enqueue(1, "one", true).unwrap();
        queue.enqueue(2, "two", true).unwrap();
        queue.enqueue(3, "three", true).unwrap();

        assert_eq!(queue.remove(2), Some("two"));
        assert_eq!(queue.dequeue_if(|_| true), Some("one"));
        assert_eq!(queue.dequeue_if(|_| true), Some("three"));
        assert!(queue.dequeue_if(|_| true).is_none());
    }
}
