//! Periodic timer deadline selection.
//!
//! This module keeps the pure deadline calculation separate from architecture timer programming.

/// Selects the next timer deadline and the following periodic deadline.
///
/// A zero or expired scheduled deadline restarts the cadence one interval after `now_ns`.
pub(super) const fn select_next_deadline(
    now_ns: u64,
    scheduled_ns: u64,
    interval_ns: u64,
) -> (u64, u64) {
    let deadline_ns = if scheduled_ns == 0 || now_ns >= scheduled_ns {
        now_ns.saturating_add(interval_ns)
    } else {
        scheduled_ns
    };
    let following_ns = deadline_ns.saturating_add(interval_ns);
    (deadline_ns, following_ns)
}

/// Periodic timer deadline selection tests.
///
/// These cases cover initial scheduling, cadence preservation, and missed deadlines.
#[cfg(test)]
mod tests {
    use super::select_next_deadline;

    /// Verifies that an uninitialized schedule starts after the current time.
    ///
    /// The following deadline remains one interval after the programmed deadline.
    #[test]
    fn initial_deadline_starts_one_interval_after_now() {
        assert_eq!(select_next_deadline(1_000, 0, 10), (1_010, 1_020));
    }

    /// Verifies that a future schedule preserves its established cadence.
    ///
    /// Handler latency before the deadline does not shift either returned deadline.
    #[test]
    fn future_deadline_preserves_periodic_cadence() {
        assert_eq!(select_next_deadline(1_011, 1_020, 10), (1_020, 1_030));
    }

    /// Verifies that an expired schedule skips missed periodic timer events.
    ///
    /// Restarting after the current time avoids a catch-up interrupt storm.
    #[test]
    fn expired_deadline_skips_missed_timer_events() {
        assert_eq!(select_next_deadline(1_050, 1_020, 10), (1_060, 1_070));
    }
}
