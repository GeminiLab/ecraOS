//! Fixed physical scheduler queue state.
//!
//! Each CPU owns one physical FIFO queue. Its domain attachment changes transactionally, while
//! queue storage remains local to the CPU and no cross-domain idle pull is permitted.

use alloc::{collections::VecDeque, vec::Vec};

use super::DomainId;

/// The attachment state of one physical CPU queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueState {
    /// The queue accepts tasks for this active domain.
    Active(DomainId),
    /// The queue is draining its former domain and rejects publication.
    Draining(DomainId),
    /// The queue accepts no placement or idle-pull selection.
    Inactive,
}

/// A fixed physical FIFO queue with an explicit attachment state.
pub struct PhysicalQueue<T> {
    state: QueueState,
    entries: VecDeque<T>,
}

impl<T> PhysicalQueue<T> {
    /// Creates an inactive empty physical queue.
    pub const fn new() -> Self {
        Self {
            state: QueueState::Inactive,
            entries: VecDeque::new(),
        }
    }

    /// Returns the current attachment state.
    pub const fn state(&self) -> QueueState {
        self.state
    }

    /// Activates an empty queue for one domain.
    pub fn activate(&mut self, domain: DomainId) -> Result<(), QueueStateError> {
        if self.state != QueueState::Inactive || !self.entries.is_empty() {
            return Err(QueueStateError::NotInactive);
        }
        self.state = QueueState::Active(domain);
        Ok(())
    }

    /// Starts draining an active queue for its attached domain.
    pub fn begin_drain(&mut self, domain: DomainId) -> Result<(), QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        self.state = QueueState::Draining(domain);
        Ok(())
    }

    /// Completes draining after every entry has been transferred.
    pub fn finish_drain(&mut self, domain: DomainId) -> Result<(), QueueStateError> {
        if self.state != QueueState::Draining(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        if !self.entries.is_empty() {
            return Err(QueueStateError::NotEmpty);
        }
        self.state = QueueState::Inactive;
        Ok(())
    }

    /// Appends one task to the active queue of its domain.
    pub fn push(&mut self, domain: DomainId, entry: T) -> Result<(), QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        self.entries.push_back(entry);
        Ok(())
    }

    /// Appends a FIFO batch to the active queue of its domain.
    ///
    /// The caller drains a source queue before invoking this operation, so extending the backing
    /// deque preserves the source ordering at the destination tail.
    pub fn append_fifo(
        &mut self,
        domain: DomainId,
        entries: VecDeque<T>,
    ) -> Result<(), QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        self.entries.extend(entries);
        Ok(())
    }

    /// Returns whether the queue contains no runnable task.
    ///
    /// The result is meaningful only while the caller retains the queue lock and has revalidated
    /// its attachment state.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of queued runnable tasks.
    ///
    /// The result is an advisory snapshot used only while the caller retains the queue lock.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reserves queue capacity before a draining transaction begins.
    ///
    /// A domain-switch transaction invokes this on every destination before the source enters
    /// `Draining`, which lets it fail without changing queue attachment state.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), QueueStateError> {
        self.entries
            .try_reserve(additional)
            .map_err(|_| QueueStateError::AllocationFailed)
    }

    /// Pops the oldest task from the active queue of its domain.
    pub fn pop(&mut self, domain: DomainId) -> Result<Option<T>, QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        Ok(self.entries.pop_front())
    }

    /// Steals one newest task from an active queue of the same domain.
    pub fn steal_tail(&mut self, domain: DomainId) -> Result<Option<T>, QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        Ok(self.entries.pop_back())
    }

    /// Removes the first matching entry from the active queue of its domain.
    ///
    /// This narrow deterministic lookup supports the compatibility `yield_to` API. Normal
    /// scheduling always uses [`Self::pop`] and therefore remains FIFO.
    pub fn take_matching(
        &mut self,
        domain: DomainId,
        matches: impl FnMut(&T) -> bool,
    ) -> Result<Option<T>, QueueStateError> {
        if self.state != QueueState::Active(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        let index = self.entries.iter().position(matches);
        Ok(index.and_then(|index| self.entries.remove(index)))
    }

    /// Removes every entry in FIFO order while the queue is draining.
    pub fn drain_fifo(&mut self, domain: DomainId) -> Result<VecDeque<T>, QueueStateError> {
        if self.state != QueueState::Draining(domain) {
            return Err(QueueStateError::WrongDomain);
        }
        Ok(core::mem::take(&mut self.entries))
    }

    /// Redistributes FIFO entries across active destinations in round-robin order.
    pub fn redistribute_fifo(entries: VecDeque<T>, destinations: &[usize]) -> Vec<Vec<T>> {
        let mut result = (0..destinations.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        if destinations.is_empty() {
            return result;
        }
        for (index, entry) in entries.into_iter().enumerate() {
            result[index % destinations.len()].push(entry);
        }
        result
    }
}

/// Errors returned by physical queue state transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueStateError {
    /// The queue was not inactive and empty when activation was requested.
    NotInactive,
    /// The supplied domain did not match the queue attachment.
    WrongDomain,
    /// A drain completion was requested while entries remained.
    NotEmpty,
    /// Queue backing storage could not reserve required transaction capacity.
    AllocationFailed,
}

#[cfg(test)]
mod tests {
    use super::{PhysicalQueue, QueueState, QueueStateError};

    #[test]
    fn drains_in_original_fifo_order() {
        let mut queue = PhysicalQueue::new();
        queue.activate(7).unwrap();
        queue.push(7, 1).unwrap();
        queue.push(7, 2).unwrap();
        queue.begin_drain(7).unwrap();
        assert_eq!(
            queue
                .drain_fifo(7)
                .unwrap()
                .into_iter()
                .collect::<alloc::vec::Vec<_>>(),
            [1, 2]
        );
        queue.finish_drain(7).unwrap();
        assert_eq!(queue.state(), QueueState::Inactive);
        assert_eq!(queue.push(7, 3), Err(QueueStateError::WrongDomain));
    }

    #[test]
    fn rejects_cross_domain_tail_pull() {
        let mut queue = PhysicalQueue::new();
        queue.activate(7).unwrap();
        queue.push(7, 1).unwrap();
        assert_eq!(queue.steal_tail(8), Err(QueueStateError::WrongDomain));
        assert_eq!(queue.steal_tail(7), Ok(Some(1)));
    }

    #[test]
    fn draining_queue_accepts_fifo_transfer_before_deactivation() {
        let mut source = PhysicalQueue::new();
        let mut destination = PhysicalQueue::new();
        source.activate(7).unwrap();
        destination.activate(7).unwrap();
        source.push(7, 1).unwrap();
        source.push(7, 2).unwrap();
        source.begin_drain(7).unwrap();

        destination
            .append_fifo(7, source.drain_fifo(7).unwrap())
            .unwrap();
        source.finish_drain(7).unwrap();

        assert_eq!(destination.pop(7), Ok(Some(1)));
        assert_eq!(destination.pop(7), Ok(Some(2)));
        assert_eq!(source.state(), QueueState::Inactive);
    }
}
