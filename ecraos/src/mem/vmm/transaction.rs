//! Kernel page-table mapping transactions.

use alloc::vec::Vec;
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

use expanic::bug;
use expt::{
    error::PagingResult,
    opaque::{OpaquePageTable, PageTableAction},
    pt::cleanup::TlbFlushes,
};
use exspin::RwLockNoIrq;
use log::warn;
use memory_addr::VirtAddr;

use crate::{
    mp::{self, LogicalCpuId},
    tlb::{self, TlbShootdownError},
};

#[cfg(test)]
mod tests;

use super::{
    VIRTUAL_ADDRESS_SPACE,
    direct_mapping::FinalRegions,
    finish_local_cleanup,
    state::{FormalPhase, StableFormalPhase, VmmState},
};

/// The next [`MappingTransaction`] identifier.
static NEXT_MAPPING_TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);

/// An exclusive transaction for kernel page-table updates.
///
/// Keeps other mapping transactions from starting while page-table changes, TLB invalidation,
/// and mapping-record updates are in progress.
///
/// # Usage
///
/// 1. Call [`Self::begin`] or [`Self::try_begin`] to begin a transaction.
/// 2. Call [`Self::apply_actions`] to change mappings and invalidate TLB on the current CPU.
/// 3. Call [`Self::perform_remote_flushes`] to complete the recorded TLB invalidations on other
///    CPUs.
/// 4. Do any other necessary work, then drop the transaction.
///
/// A transaction can perform several rounds of mapping changes and TLB invalidation.
/// [`Self::perform_remote_flushes`] leaves the transaction active. Dropping it ends the transaction
/// and restores the [`StableFormalPhase`] saved at the start. Use [`Self::finish_to`] to end the
/// transaction and move to a new phase.
///
/// Dropping a transaction automatically flushes any pending remote TLB invalidations. This is
/// not recommended, as it may wait or panic implicitly. Use [`Self::perform_remote_flushes`]
/// explicitly instead. [`Self::discard_remote_flushes`] discards pending requests without
/// performing them, subject to its safety requirements.
///
/// # Execution context
///
/// Transactions must not be nested. Preemption is disabled for the transaction's lifetime.
/// Interrupts are locally disabled only when a method of this type needs to update the VMM state
/// and/or the page table, such as [`Self::begin`], [`Self::try_begin`], [`Self::apply_actions`],
/// [`Self::drop`], and [`Self::finish_to`]. See documentation for each method for details.
///
/// # Failure behavior
///
/// Transactions do not roll back page-table changes. Before marking changed mappings available for
/// use or reusing affected physical frames, callers must complete the required local and remote
/// invalidation. If invalidation is incomplete, those frames must not be released for reuse.
pub struct MappingTransaction {
    /// Other CPUs that must invalidate TLB entries changed by this transaction.
    targets: Vec<LogicalCpuId>,
    /// Requests awaiting successful invalidation on the target CPUs.
    remote_flushes: TlbFlushes<VirtAddr>,
    /// The preemption guard that disables preemption for the transaction's lifetime.
    _guard: kernel_guard::NoPreempt,
    /// The ID of this transaction.
    ///
    /// Used to detect illegal state transitions during the transaction's lifetime.
    transaction_id: u64,
    /// The [`StableFormalPhase`] to restore when the transaction ends.
    ///
    /// Initially the phase saved at the start, unless replaced by [`Self::finish_to`].
    pub(super) previous_phase: StableFormalPhase,
    /// Marker field to make this struct `!Send` and `!Sync`.
    _local: PhantomData<*mut ()>,
}

/// Operations for changing and completing kernel page-table mappings.
impl MappingTransaction {
    /// Begins a kernel page-table mapping transaction.
    ///
    /// Waits for any existing mapping transaction to end. Selects other online CPUs whose
    /// interprocessor interrupt handlers are initialized as targets for this transaction's
    /// TLB invalidation.
    ///
    /// Once the current CPU is marked IPI-ready, local interrupts must be enabled when calling
    /// this method. While waiting for another mapping transaction to end, this CPU may need to
    /// handle that transaction's TLB invalidation interrupts.
    ///
    /// See the type's [execution context](MappingTransaction#execution-context) for lifetime rules.
    ///
    /// # Errors
    ///
    /// Returns [`TlbShootdownError::InvalidRequest`] if VMM has not switched to the formal
    /// page-table allocator. Returns [`TlbShootdownError::Ipi`] with
    /// [`InterruptsDisabled`](crate::ipi::IpiError::InterruptsDisabled) if the interrupt
    /// requirement above is not met.
    pub fn begin() -> Result<Self, TlbShootdownError> {
        match Self::begin_inner(true)? {
            Some(transaction) => Ok(transaction),
            None => bug!("waiting acquisition returned without beginning a transaction"),
        }
    }

    /// Attempts to begin a kernel page-table mapping transaction without waiting.
    ///
    /// Returns `Ok(None)` if the VMM lock is occupied or another mapping transaction is active.
    /// Returns `Ok(Some(transaction))` after acquiring the transaction. A failed attempt leaves
    /// the VMM phase and the caller's interrupt and preemption state unchanged.
    ///
    /// Unlike [`Self::begin`], this method permits disabled local interrupts because it does not
    /// wait for another transaction. Completing remote invalidations still requires the execution
    /// context documented by [`Self::perform_remote_flushes`].
    ///
    /// # Errors
    ///
    /// Returns [`TlbShootdownError::InvalidRequest`] if VMM has not switched to the formal
    /// page-table allocator.
    pub fn try_begin() -> Result<Option<Self>, TlbShootdownError> {
        Self::begin_inner(false)
    }

    /// Prepares transaction state and acquires ownership, optionally waiting for exclusive access.
    ///
    /// `wait` enables the interrupt-state requirement and waits for the VMM lock and any existing
    /// transaction. Otherwise, a contended attempt returns `Ok(None)` and releases its guard.
    fn begin_inner(wait: bool) -> Result<Option<Self>, TlbShootdownError> {
        let address_space = VIRTUAL_ADDRESS_SPACE
            .get()
            .ok_or(TlbShootdownError::InvalidRequest)?;
        let targets_and_self = mp::ipi_ready_cpu_ids();
        let no_preempt_guard = kernel_guard::NoPreempt::new();
        if wait && mp::is_ipi_ready(mp::current_cpu_id()) && !exarch::trap::local_enabled() {
            return Err(TlbShootdownError::Ipi(
                crate::ipi::IpiError::InterruptsDisabled,
            ));
        }
        let targets = tlb::remote_targets(targets_and_self, mp::current_cpu_id());
        let transaction_id = NEXT_MAPPING_TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
        let Some(previous_phase) = acquire_transition(&address_space.state, transaction_id, wait)?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            targets,
            remote_flushes: TlbFlushes::None,
            _guard: no_preempt_guard,
            transaction_id,
            previous_phase,
            _local: PhantomData,
        }))
    }

    /// Performs recorded TLB invalidations on other CPUs and waits for completion.
    ///
    /// Processes requests recorded by [`Self::apply_actions`] and [`Self::append_remote_flushes`].
    /// Uses the target CPUs selected at the start. Successful completion clears the requests.
    /// An error retains them for a later attempt. An empty batch requires no interrupts.
    ///
    /// When there are pending requests and target CPUs, local interrupts must be enabled and the
    /// caller must hold no locks needed by those CPUs. An empty target set requires no interrupts.
    /// The transaction remains active after this method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if the request is invalid or the target CPUs cannot complete the requested
    /// invalidation. See the type's [failure behavior](MappingTransaction#failure-behavior).
    pub fn perform_remote_flushes(&mut self) -> Result<(), TlbShootdownError> {
        tlb::shootdown(&self.targets, &self.remote_flushes)?;
        self.remote_flushes.clear();
        Ok(())
    }

    /// Discards all pending remote TLB invalidations.
    ///
    /// Does not perform invalidation or end the transaction. Later mapping changes continue to
    /// record remote invalidation requests normally.
    ///
    /// # Safety
    ///
    /// The caller must ensure that discarding all pending requests cannot let remote CPUs use
    /// stale translations or page-walk state. Any required remote invalidation must be completed
    /// by other means before those CPUs access the affected mappings or affected frames are
    /// reused.
    pub unsafe fn discard_remote_flushes(&mut self) {
        self.remote_flushes.clear();
    }

    /// Adds remote TLB invalidation requests without executing them.
    ///
    /// Use to request additional TLB invalidations beyond the page-table cleanup, such as a full
    /// flush. Local TLB invalidations should be handled separately by the caller if necessary,
    /// before the next call to [`Self::perform_remote_flushes`].
    ///
    /// Provided virtual addresses must be valid within the address space of the current CPU, and
    /// must be aligned to the base page size.
    pub fn append_remote_flushes(&mut self, requests: TlbFlushes<VirtAddr>) {
        self.remote_flushes.append(requests);
    }

    /// Returns the target CPU list for this transaction's remote TLB invalidations.
    pub fn targets(&self) -> &[LogicalCpuId] {
        &self.targets
    }

    /// Applies mapping changes and invalidates affected TLB entries on the current CPU.
    ///
    /// Records the corresponding remote TLB invalidations for [`Self::perform_remote_flushes`].
    /// Requests accumulate across calls until performed successfully or explicitly discarded.
    ///
    /// The caller must prevent concurrent accesses to the affected ranges as required by
    /// [expt's page-table update contract](expt#safely-updating-page-tables).
    ///
    /// # Errors
    ///
    /// Returns a page-table error if an action fails. Earlier changes may remain applied. Their
    /// local TLB invalidation is completed before returning, and a full remote invalidation is
    /// recorded because the failed operation does not return its cleanup batch. See the type's
    /// [failure behavior](MappingTransaction#failure-behavior).
    pub fn apply_actions<I>(&mut self, actions: I) -> PagingResult<VirtAddr>
    where
        I: IntoIterator<Item = PageTableAction<VirtAddr>>,
    {
        self.with_page_table_mut(|table, _, remote_flushes| {
            match table.apply_actions_with_cleanup(actions) {
                Ok(cleanup) => {
                    finish_local_cleanup(cleanup, remote_flushes);
                    Ok(())
                }
                Err(error) => {
                    remote_flushes.append(TlbFlushes::Full);
                    Err(error)
                }
            }
        })
    }

    /// Runs a callback with mutable access to the kernel page table and direct mapping records.
    ///
    /// Holds the VMM write lock with local interrupts and preemption disabled throughout the
    /// callback. The callback must not recursively acquire this lock or wait for another CPU.
    /// The callback also receives this transaction's pending remote invalidations. Page-table
    /// changes must finish local invalidation and record their remote requests before returning.
    pub(super) fn with_page_table_mut<F, T>(&mut self, function: F) -> T
    where
        F: FnOnce(
            &mut OpaquePageTable<VirtAddr>,
            &mut FinalRegions,
            &mut TlbFlushes<VirtAddr>,
        ) -> T,
    {
        let mut state = VIRTUAL_ADDRESS_SPACE.state.write();
        let VmmState::Formal {
            page_table,
            mappings,
            phase: FormalPhase::Transitioning { transaction_id },
        } = &mut *state
        else {
            bug!("vmm state changed while a mapping transaction is active");
        };
        if *transaction_id != self.transaction_id {
            bug!("transaction id mismatch");
        }
        function(page_table, mappings, &mut self.remote_flushes)
    }

    /// Ends the transaction and sets the VMM phase to `next`.
    ///
    /// Use when an update changes the identity-mapping phase, such as removing identity mappings.
    /// Ordinary dropping restores the phase saved at the start. Call after page-table changes,
    /// mapping-record updates, and all required TLB invalidations have completed.
    pub fn finish_to(mut self, next: StableFormalPhase) {
        self.previous_phase = next;
    }
}

/// Transaction release and restoration of the recorded VMM phase.
impl Drop for MappingTransaction {
    /// Ends the transaction and restores its recorded VMM phase.
    ///
    /// Restores the phase saved when the transaction began, or the one selected by
    /// [`Self::finish_to`]. Leaves the page table and mapping records unchanged.
    fn drop(&mut self) {
        if !self.remote_flushes.is_empty() {
            warn!("MappingTransaction dropped with pending remote TLB invalidations.");
            warn!("MappingTransaction processes remaining remote TLB invalidations automatically.");
            warn!(
                "However, this is not recommended. Use `perform_remote_flushes` explicitly instead."
            );

            self.perform_remote_flushes()
                .expect("failed to flush remote TLB invalidations automatically");
        }

        let mut state = VIRTUAL_ADDRESS_SPACE.state.write();
        let VmmState::Formal { phase, .. } = &mut *state else {
            panic!("mapping transaction lost its formal state");
        };
        assert!(phase.finish_transition(self.transaction_id, self.previous_phase));
    }
}

/// Claims the formal VMM phase for a transaction, waiting only when `wait` is true.
///
/// Returns the previous stable phase on success, `Ok(None)` on contention without waiting, or
/// [`TlbShootdownError::InvalidRequest`] when the page-table allocator is not yet formal.
fn acquire_transition(
    state: &RwLockNoIrq<VmmState>,
    transaction_id: u64,
    wait: bool,
) -> Result<Option<StableFormalPhase>, TlbShootdownError> {
    loop {
        let mut locked = if wait {
            state.write()
        } else if let Some(locked) = state.try_write() {
            locked
        } else {
            return Ok(None);
        };
        let VmmState::Formal { phase, .. } = &mut *locked else {
            return Err(TlbShootdownError::InvalidRequest);
        };
        if let Some(previous) = phase.begin_transition(transaction_id) {
            return Ok(Some(previous));
        }
        if !wait {
            return Ok(None);
        }
        // The current owner may be waiting for this CPU's TLB interrupt. Release the lock to
        // restore the caller's interrupt state before waiting for that transaction.
        drop(locked);
        core::hint::spin_loop();
    }
}
