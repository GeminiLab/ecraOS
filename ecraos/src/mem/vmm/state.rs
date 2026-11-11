//! VMM lifecycle states.

use core::mem;

use expt::opaque::{OpaquePageTable, OpaquePageTableType};
use memory_addr::VirtAddr;

use super::direct_mapping::FinalRegions;

#[cfg(test)]
mod tests;

/// A [`FormalPhase`] that is stable (not transitioning).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StableFormalPhase {
    /// Same as [`FormalPhase::IdentityActive`].
    IdentityActive,
    /// Same as [`FormalPhase::Active`].
    Active,
}

/// A phase of [`VmmState::Formal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormalPhase {
    /// The formal page table is active on the hardware and contains identity mappings.
    IdentityActive,
    /// A mapping transaction is updating the formal page table or completing its invalidations.
    Transitioning {
        /// ID of the transaction currently changing the page table.
        transaction_id: u64,
    },
    /// The formal page table is active on the hardware and contains no identity mappings.
    Active,
}

/// State updates at the beginning and end of a formal page-table update.
impl FormalPhase {
    /// Begins an exclusive formal page-table update and returns the previous stable phase.
    ///
    /// Returns [`None`] without changing the phase if another transaction is in progress.
    pub fn begin_transition(&mut self, transaction_id: u64) -> Option<StableFormalPhase> {
        let stable = match *self {
            Self::IdentityActive => StableFormalPhase::IdentityActive,
            Self::Active => StableFormalPhase::Active,
            Self::Transitioning { .. } => return None,
        };
        *self = Self::Transitioning { transaction_id };
        Some(stable)
    }

    /// Ends an exclusive formal page-table update owned by `transaction_id`.
    ///
    /// Sets the phase to `next` and returns `true` on success. Returns `false` without changing
    /// the phase if the transaction does not match.
    pub fn finish_transition(&mut self, transaction_id: u64, next: StableFormalPhase) -> bool {
        if *self != (Self::Transitioning { transaction_id }) {
            return false;
        }
        *self = match next {
            StableFormalPhase::IdentityActive => Self::IdentityActive,
            StableFormalPhase::Active => Self::Active,
        };
        true
    }
}

/// A phase of [`VmmState::Early`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EarlyPhase {
    /// The early page table is ready, but is not active on the hardware.
    TableReady,
    /// The early page table is active on the hardware and contains identity mappings.
    Active,
}

/// State updates at the beginning of early page-table activation.
impl EarlyPhase {
    /// Marks the early page table as active and returns `true` on success.
    ///
    /// Returns `false` without changing the phase if the early page table was already marked
    /// active.
    pub fn activate(&mut self) -> bool {
        if *self != Self::TableReady {
            return false;
        }
        *self = Self::Active;
        true
    }
}

/// The current VMM lifecycle state and its page-table data.
pub enum VmmState {
    /// The address layout is ready, but the first page table has not been created.
    LayoutReady {
        /// The early page-table type, using the early page allocator.
        page_table_type: OpaquePageTableType<VirtAddr>,
    },
    /// The early page table has been created and may or may not be active on the hardware.
    Early {
        /// The early page-table, using the early page allocator.
        page_table: OpaquePageTable<VirtAddr>,
        /// The phase of the early page table, indicating whether it is active on the hardware.
        phase: EarlyPhase,
    },
    /// The formal page table has been created and is active on the hardware.
    Formal {
        /// The formal page table, using the buddy allocator for new table frames.
        page_table: OpaquePageTable<VirtAddr>,
        /// Page-aligned direct-mapping records, including any retained `NO_MAP` ranges.
        mappings: FinalRegions,
        /// The phase of the formal page table, indicating whether identity mappings remain or a
        /// page-table change is in progress.
        phase: FormalPhase,
    },
}

/// Transfer of the early page table to the formal allocator.
impl VmmState {
    /// Switches to the formal page table allocator without changing the page table hierarchy.
    ///
    /// Preserves the existing hierarchy and starts the formal phase with identity mappings active.
    /// `mappings` must describe the direct mappings installed in that hierarchy.
    ///
    /// # Safety
    ///
    /// The descriptor must match the existing page-table hierarchy. Its allocator must be able to
    /// access and deallocate every frame in that hierarchy.
    ///
    /// # Panics
    ///
    /// Panics if the VMM does not own an active early page table.
    pub unsafe fn switch_to_formal_page_table(
        &mut self,
        page_table_type: OpaquePageTableType<VirtAddr>,
        mappings: FinalRegions,
    ) {
        let Self::Early {
            page_table,
            phase: EarlyPhase::Active,
        } = self
        else {
            panic!("formal handoff requires an active early page table");
        };
        let root = mem::replace(page_table, OpaquePageTable::dummy()).leak();
        // SAFETY: Leaking the only owner preserves the hierarchy without calling its old methods.
        // The caller guarantees that the descriptor matches and can manage every table frame.
        let page_table = unsafe { page_table_type.new_pagetable_at(root) };
        *self = Self::Formal {
            page_table,
            mappings,
            phase: FormalPhase::IdentityActive,
        };
    }

    /// Returns a short description of the current state.
    pub const fn desc(&self) -> &'static str {
        match self {
            Self::LayoutReady { .. } => "layout ready",
            Self::Early { phase, .. } => match phase {
                EarlyPhase::TableReady => "early page table ready",
                EarlyPhase::Active => "early page table active",
            },
            Self::Formal { phase, .. } => match phase {
                FormalPhase::IdentityActive => "formal and identity mappings active",
                FormalPhase::Transitioning { .. } => "formal mapping in transition",
                FormalPhase::Active => "formal mappings active",
            },
        }
    }
}
