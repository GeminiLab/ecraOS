//! Transaction acquisition and contention tests.

use std::time::{Duration, Instant};

use expt::opaque::OpaquePageTable;
use exspin::RwLockNoIrq;

use super::{
    FinalRegions, FormalPhase, StableFormalPhase, TlbShootdownError, VmmState, acquire_transition,
};

/// Creates a formal VMM state with a dummy page table for testing.
fn formal_state(phase: FormalPhase) -> RwLockNoIrq<VmmState> {
    RwLockNoIrq::new(VmmState::Formal {
        page_table: OpaquePageTable::dummy(),
        mappings: FinalRegions::new(),
        phase,
    })
}

/// Verifies that nonwaiting acquisition leaves the phase unchanged when a read lock is held.
#[test]
fn nonwaiting_acquisition_rejects_lock_contention() {
    let state = formal_state(FormalPhase::Active);
    let reader = state.read();
    assert_eq!(acquire_transition(&state, 9, false), Ok(None));
    assert!(matches!(
        &*reader,
        VmmState::Formal {
            phase: FormalPhase::Active,
            ..
        }
    ));
}

/// Verifies that nonwaiting acquisition preserves the active transaction's ID.
#[test]
fn nonwaiting_acquisition_preserves_existing_owner() {
    let state = formal_state(FormalPhase::Transitioning { transaction_id: 7 });
    assert_eq!(acquire_transition(&state, 9, false), Ok(None));
    assert!(matches!(
        &*state.read(),
        VmmState::Formal {
            phase: FormalPhase::Transitioning { transaction_id: 7 },
            ..
        }
    ));
}

/// Verifies that nonwaiting acquisition records its ID and returns the previous stable phase.
#[test]
fn nonwaiting_acquisition_saves_phase_and_claims_ownership() {
    for (phase, saved) in [
        (
            FormalPhase::IdentityActive,
            StableFormalPhase::IdentityActive,
        ),
        (FormalPhase::Active, StableFormalPhase::Active),
    ] {
        let state = formal_state(phase);
        assert_eq!(acquire_transition(&state, 9, false), Ok(Some(saved)));
        assert!(matches!(
            &*state.read(),
            VmmState::Formal {
                phase: FormalPhase::Transitioning { transaction_id: 9 },
                ..
            }
        ));
    }
}

/// Verifies that both acquisition modes reject the early VMM state.
#[test]
fn acquisition_rejects_early_page_tables() {
    let state = RwLockNoIrq::new(VmmState::Early {
        page_table: OpaquePageTable::dummy(),
        phase: super::super::state::EarlyPhase::Active,
    });
    for wait in [false, true] {
        assert_eq!(
            acquire_transition(&state, 9, wait),
            Err(TlbShootdownError::InvalidRequest)
        );
    }
}

/// Verifies that waiting acquisition releases the lock so the active transaction can finish.
#[test]
fn waiting_acquisition_allows_the_previous_owner_to_finish() {
    let state = formal_state(FormalPhase::Transitioning { transaction_id: 7 });
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| acquire_transition(&state, 9, true));
        let deadline = Instant::now() + Duration::from_secs(2);
        // Observe the waiting thread holding the lock while transaction 7 is still active.
        // Only that thread can prevent this thread from acquiring the lock here.
        let observed = loop {
            if state.try_write().is_none() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        let mut locked = state.write();
        let VmmState::Formal { phase, .. } = &mut *locked else {
            unreachable!()
        };
        assert!(phase.finish_transition(7, StableFormalPhase::Active));
        drop(locked);
        assert_eq!(waiter.join().unwrap(), Ok(Some(StableFormalPhase::Active)));
        assert!(observed, "waiter never inspected the active transaction");
    });
}
