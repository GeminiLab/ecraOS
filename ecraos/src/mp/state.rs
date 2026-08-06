//! Secondary CPU lifecycle states.
//!
//! The state machine is intentionally dependency-free so host tests can validate invalid
//! transitions without building the no-std kernel.

/// The lifecycle state of a CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuState {
    /// The CPU has not received a startup request.
    Offline,
    /// The architecture accepted a startup request and initialization is pending.
    Starting,
    /// The CPU completed the required per-CPU initialization sequence.
    Online,
    /// Startup failed or exceeded its deadline.
    Failed,
}

impl CpuState {
    /// Checks whether a lifecycle state transition is valid.
    pub const fn can_transition_to(self, to: CpuState) -> bool {
        matches!(
            (self, to),
            (CpuState::Offline, CpuState::Starting)
                | (CpuState::Starting, CpuState::Online)
                | (CpuState::Starting, CpuState::Failed)
        )
    }

    /// Converts the lifecycle state to its atomic storage encoding.
    ///
    /// The returned value is stable for values stored in the per-CPU state array.
    pub const fn to_u8(self) -> u8 {
        match self {
            CpuState::Offline => 0,
            CpuState::Starting => 1,
            CpuState::Online => 2,
            CpuState::Failed => 3,
        }
    }

    /// Converts an atomic storage encoding to its lifecycle state.
    ///
    /// This function panics when `value` is not an encoding produced by [`CpuState::to_u8`].
    pub const fn from_u8(value: u8) -> CpuState {
        match value {
            0 => CpuState::Offline,
            1 => CpuState::Starting,
            2 => CpuState::Online,
            3 => CpuState::Failed,
            _ => panic!("Invalid CPU state value"),
        }
    }
}

/// CPU lifecycle state-machine tests.
///
/// These tests cover valid transitions, rejected transitions, and atomic storage encoding.
#[cfg(test)]
mod tests {
    use super::CpuState;

    /// Verifies that an offline CPU can enter the startup sequence.
    ///
    /// Starting is the only valid state immediately following offline.
    #[test]
    fn offline_starts_as_starting() {
        assert!(CpuState::Offline.can_transition_to(CpuState::Starting),);
    }

    /// Verifies that an offline CPU cannot publish itself as online.
    ///
    /// Every secondary CPU must pass through the starting state first.
    #[test]
    fn offline_cannot_skip_starting() {
        assert!(!CpuState::Offline.can_transition_to(CpuState::Online),);
    }

    /// Verifies that a starting CPU can publish online exactly once.
    ///
    /// An online CPU cannot repeat the publication transition.
    #[test]
    fn starting_can_be_online_once() {
        assert!(CpuState::Starting.can_transition_to(CpuState::Online),);
        assert!(!CpuState::Online.can_transition_to(CpuState::Online),);
    }

    /// Verifies that a failed CPU cannot later publish itself as online.
    ///
    /// Failed is a terminal lifecycle state for the current startup attempt.
    #[test]
    fn failed_cannot_become_online() {
        assert!(CpuState::Starting.can_transition_to(CpuState::Failed),);
        assert!(!CpuState::Failed.can_transition_to(CpuState::Online),);
    }

    /// Verifies that every lifecycle state survives atomic encoding and decoding.
    ///
    /// This guards the representation shared by initialization, loads, and transitions.
    #[test]
    fn states_round_trip_through_atomic_encoding() {
        for state in [
            CpuState::Offline,
            CpuState::Starting,
            CpuState::Online,
            CpuState::Failed,
        ] {
            assert_eq!(CpuState::from_u8(state.to_u8()), state);
        }
    }
}
