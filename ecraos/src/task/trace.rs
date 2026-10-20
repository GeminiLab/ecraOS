//! Bounded scheduler trace records.
//!
//! The ring is allocation-free and intended for debug or test builds. Writers overwrite the
//! oldest record after capacity is reached and account every overwrite.

/// A scheduler trace event category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceEventKind {
    /// A task became runnable.
    Enqueue,
    /// A task began executing.
    Switch,
    /// A task blocked.
    Block,
    /// A task exited.
    Exit,
    /// A timer or remote path requested a safe-point reschedule.
    ReschedulePending,
}

/// One compact scheduler trace record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceEvent {
    /// Monotonic per-CPU sequence number.
    pub sequence: u64,
    /// Event category.
    pub kind: TraceEventKind,
    /// Task identity associated with the event.
    pub task_id: u64,
    /// Domain identity associated with the event.
    pub domain_id: u64,
}

/// A fixed-capacity overwrite ring for scheduler trace records.
pub struct TraceRing<const CAPACITY: usize> {
    entries: [Option<TraceEvent>; CAPACITY],
    next: usize,
    sequence: u64,
    dropped: u64,
}

impl<const CAPACITY: usize> TraceRing<CAPACITY> {
    /// Creates an empty trace ring.
    pub const fn new() -> Self {
        Self {
            entries: [None; CAPACITY],
            next: 0,
            sequence: 0,
            dropped: 0,
        }
    }

    /// Appends one event and returns its assigned sequence number.
    pub fn push(&mut self, kind: TraceEventKind, task_id: u64, domain_id: u64) -> u64 {
        assert!(CAPACITY != 0, "trace ring capacity must be positive");
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        if self.entries[self.next].is_some() {
            self.dropped = self.dropped.saturating_add(1);
        }
        self.entries[self.next] = Some(TraceEvent {
            sequence,
            kind,
            task_id,
            domain_id,
        });
        self.next = (self.next + 1) % CAPACITY;
        sequence
    }

    /// Returns the number of overwritten records.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Returns a chronological snapshot without allocating.
    pub fn snapshot<const OUT: usize>(&self) -> ([Option<TraceEvent>; OUT], usize) {
        let mut output = [None; OUT];
        let mut count = 0;
        let start = if self.entries.iter().all(Option::is_some) {
            self.next
        } else {
            0
        };
        for offset in 0..CAPACITY {
            if count == OUT {
                break;
            }
            if let Some(event) = self.entries[(start + offset) % CAPACITY] {
                output[count] = Some(event);
                count += 1;
            }
        }
        (output, count)
    }
}

impl<const CAPACITY: usize> Default for TraceRing<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{TraceEventKind, TraceRing};

    #[test]
    fn overwrite_and_snapshot_are_chronological() {
        let mut ring = TraceRing::<2>::new();
        ring.push(TraceEventKind::Enqueue, 1, 2);
        ring.push(TraceEventKind::Switch, 3, 4);
        ring.push(TraceEventKind::Exit, 5, 6);
        let (events, count) = ring.snapshot::<2>();
        assert_eq!(count, 2);
        assert_eq!(events[0].unwrap().task_id, 3);
        assert_eq!(events[1].unwrap().task_id, 5);
        assert_eq!(ring.dropped(), 1);
    }
}
