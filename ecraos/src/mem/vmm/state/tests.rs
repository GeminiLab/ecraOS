//! Lifecycle transition tests for the VMM state.

use expt::opaque::{OpaquePageTable, OpaquePageTableType};

use crate::mem::vmm::direct_mapping::FinalRegions;

use super::{EarlyPhase, FormalPhase, StableFormalPhase, VmmState};

/// Verifies that a formal transition rejects competing transactions and mismatched IDs.
#[test]
fn formal_phase_serializes_transactions_by_id() {
    let mut phase = FormalPhase::IdentityActive;

    assert_eq!(
        phase.begin_transition(7),
        Some(StableFormalPhase::IdentityActive)
    );
    assert_eq!(phase, FormalPhase::Transitioning { transaction_id: 7 });
    assert_eq!(phase.begin_transition(8), None);
    assert!(!phase.finish_transition(8, StableFormalPhase::Active));
    assert!(phase.finish_transition(7, StableFormalPhase::Active));
    assert_eq!(phase, FormalPhase::Active);
    assert_eq!(phase.begin_transition(9), Some(StableFormalPhase::Active));
    assert!(phase.finish_transition(9, StableFormalPhase::Active));
}

/// Verifies that an early page table can be marked active only once.
#[test]
fn early_table_activation_rejects_repeated_publication() {
    let mut phase = EarlyPhase::TableReady;
    assert!(phase.activate());
    assert_eq!(phase, EarlyPhase::Active);
    assert!(!phase.activate());
    assert_eq!(phase, EarlyPhase::Active);
}

/// Verifies that formal handoff rejects an early table that is not marked active.
#[test]
#[should_panic(expected = "formal handoff requires an active early page table")]
fn formal_handoff_rejects_inactive_early_table() {
    let mut state = VmmState::Early {
        page_table: OpaquePageTable::dummy(),
        phase: EarlyPhase::TableReady,
    };
    // SAFETY: The inactive state is rejected before the descriptor can access any frames.
    unsafe { state.switch_to_formal_page_table(OpaquePageTableType::dummy(), FinalRegions::new()) };
}
