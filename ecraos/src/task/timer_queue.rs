//! Timer-owned sleeper and timed-wait deadline queue.
//!
//! Keeps deadline ordering and generation filtering independent from architecture timer code.

use alloc::vec::Vec;
use core::time::Duration;

/// One scheduled cooperative wakeup.
///
/// The generation distinguishes a current sleep from stale entries left by cancellation or task
/// state reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerEntry {
    /// The absolute monotonic wakeup deadline.
    pub deadline: Duration,
    /// The task identity associated with this wakeup.
    pub task_id: u64,
    /// The task's unique sleep generation.
    pub generation: u64,
    /// Stable insertion order used to preserve equal-deadline FIFO ordering.
    sequence: u64,
}

/// An ordered collection of cooperative sleeper deadlines.
///
/// Entries are kept in deadline order. The bounded Stage 2A workload favors simple ownership and
/// deterministic behavior over a heap abstraction, while stale entries remain cheap to discard.
#[derive(Debug)]
pub struct TimerQueue {
    /// Entries ordered by deadline and insertion sequence.
    entries: Vec<TimerEntry>,
    /// Sequence assigned to the next inserted entry.
    next_sequence: u64,
}

impl TimerQueue {
    /// Creates an empty deadline queue.
    ///
    /// No task owns a timer entry initially.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_sequence: 0,
        }
    }

    /// Returns the earliest scheduled deadline.
    ///
    /// Cancelled and stale entries remain visible until removed by cancellation or expiry.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.entries.first().map(|entry| entry.deadline)
    }

    /// Inserts one wakeup and reports whether the earliest deadline changed.
    ///
    /// Callers use the return value to decide whether hardware one-shot programming needs an
    /// update.
    pub fn push(&mut self, deadline: Duration, task_id: u64, generation: u64) -> bool {
        self.try_push(deadline, task_id, generation)
            .expect("timer queue allocation failed")
    }

    /// Tries to insert one wakeup without panicking on backing-storage exhaustion.
    pub fn try_push(
        &mut self,
        deadline: Duration,
        task_id: u64,
        generation: u64,
    ) -> Result<bool, ()> {
        let previous = self.next_deadline();
        let entry = TimerEntry {
            deadline,
            task_id,
            generation,
            sequence: self.next_sequence,
        };
        let index = self.entries.partition_point(|current| {
            (current.deadline, current.sequence) <= (entry.deadline, entry.sequence)
        });
        self.entries.try_reserve(1).map_err(|_| ())?;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.entries.insert(index, entry);
        Ok(previous != self.next_deadline())
    }

    /// Cancels one exact task-generation entry.
    ///
    /// Returns whether an entry was removed. A stale cancellation is harmless.
    pub fn cancel(&mut self, task_id: u64, generation: u64) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.task_id == task_id && entry.generation == generation)
        else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    /// Removes entries due by `now` and filters stale generations.
    ///
    /// The predicate receives each task identity and generation. Entries rejected by it are
    /// discarded as stale, while accepted entries are returned in deadline order.
    pub fn pop_expired(
        &mut self,
        now: Duration,
        mut current_generation: impl FnMut(u64, u64) -> bool,
    ) -> Vec<TimerEntry> {
        let due_count = self.entries.partition_point(|entry| entry.deadline <= now);
        let due = self.entries.drain(..due_count);
        due.filter(|entry| current_generation(entry.task_id, entry.generation))
            .collect()
    }
}

/// Adds a relative duration to an absolute deadline with saturation.
///
/// Saturation keeps a timer request representable even when the monotonic clock approaches its
/// maximum duration.
#[expect(unused)]
pub const fn saturating_deadline(deadline: Duration, duration: Duration) -> Duration {
    match deadline.checked_add(duration) {
        Some(value) => value,
        None => Duration::MAX,
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::TimerQueue;

    #[test]
    fn exact_cancellation_releases_one_generation() {
        let mut queue = TimerQueue::new();
        queue.push(Duration::from_nanos(10), 4, 1);
        queue.push(Duration::from_nanos(20), 4, 2);
        assert!(queue.cancel(4, 1));
        assert_eq!(queue.next_deadline(), Some(Duration::from_nanos(20)));
        assert!(!queue.cancel(4, 1));
    }
}
