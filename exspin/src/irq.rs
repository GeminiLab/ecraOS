//! Per-CPU interrupt-state coordination for non-LIFO lock guards.

use core::marker::PhantomData;

#[cfg(not(all(test, feature = "host-test")))]
use kernel_guard::IrqSave as KernelIrqSave;
use kernel_guard::{BaseGuard, NoPreempt};

#[cfg(all(test, feature = "host-test"))]
use tests::SimulatedIrqSave as KernelIrqSave;

use crate::NestedIrqError;

/// Tracks one CPU's active IRQ-safe guards and owns the original interrupt state.
///
/// Only the outermost guard owns a saved-state token. Inner guards return their temporary token
/// immediately, while the saved outer token stays in this per-CPU value until the last guard exits.
pub(crate) struct NestedIrqState<T> {
    /// The number of IRQ-safe guards currently active on this CPU.
    depth: usize,
    /// The token that restores the interrupt state from before the outermost guard entered.
    outer: Option<T>,
}

/// Empty per-CPU IRQ nesting state.
impl<T> NestedIrqState<T> {
    /// Creates an inactive IRQ nesting state.
    pub const fn new() -> Self {
        Self {
            depth: 0,
            outer: None,
        }
    }

    /// Adds one guard and retains its token only for the outermost guard.
    ///
    /// Returns an inner token for the caller to drop after releasing the per-CPU state borrow.
    /// On error, returns the token so its caller can restore execution state after ending that
    /// borrow.
    pub fn enter(&mut self, token: T) -> Result<Option<T>, (NestedIrqError, T)> {
        if self.depth == 0 {
            if self.outer.is_some() {
                return Err((NestedIrqError::UnexpectedOuterToken, token));
            }
            self.outer = Some(token);
            self.depth = 1;
            return Ok(None);
        }

        if self.outer.is_none() {
            return Err((NestedIrqError::MissingOuterToken, token));
        }
        let Some(depth) = self.depth.checked_add(1) else {
            return Err((NestedIrqError::DepthOverflow, token));
        };
        self.depth = depth;
        Ok(Some(token))
    }

    /// Removes one guard and returns the saved token only after the final guard exits.
    pub fn exit(&mut self) -> Result<Option<T>, NestedIrqError> {
        if self.depth == 0 {
            return Err(NestedIrqError::UnbalancedExit);
        }
        if self.outer.is_none() {
            return Err(NestedIrqError::MissingOuterToken);
        }

        self.depth -= 1;
        if self.depth == 0 {
            Ok(self.outer.take())
        } else {
            Ok(None)
        }
    }

    /// Returns the current number of active guards.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.depth
    }
}

/// The BSP and each AP keep independent IRQ nesting state in their per-CPU area.
#[expercpu::def_percpu]
pub(super) static IRQ_NESTING: NestedIrqState<KernelIrqSave> = NestedIrqState::new();

/// The execution policy shared by all IRQ-safe spin and reader-writer locks.
///
/// This type is public only because it appears in the public lock aliases.
#[doc(hidden)]
pub struct NoIrqGuardPolicy;

/// Per-CPU interrupt nesting for IRQ-safe lock acquisitions.
impl BaseGuard for NoIrqGuardPolicy {
    /// The saved outer token lives in per-CPU storage rather than in each lock guard.
    type State = ();

    /// Enters the shared IRQ-safe critical section on the current CPU.
    fn acquire() -> Self::State {
        // Prevent migration while resolving and mutating the current CPU's nesting state.
        NoPreempt::acquire();
        let irq_state = KernelIrqSave::new();
        // SAFETY: The temporary token disables local IRQs before the per-CPU state is accessed.
        // Preemption is already disabled, so this access remains on the same CPU.
        let entered = unsafe { IRQ_NESTING.current_ref_mut_raw().enter(irq_state) };
        match entered {
            Ok(inner_state) => drop(inner_state),
            Err((error, irq_state)) => {
                // Restore the temporary token after the mutable per-CPU borrow has ended.
                drop(irq_state);
                NoPreempt::release(());
                panic!("failed to enter the per-CPU IRQ guard: {error:?}");
            }
        }
    }

    /// Leaves the shared IRQ-safe critical section after releasing its data lock.
    fn release(_state: Self::State) {
        // SAFETY: A live outer token keeps local IRQs disabled. The matching NoPreempt guard
        // keeps this state on the CPU that entered it.
        let exited = unsafe { IRQ_NESTING.current_ref_mut_raw().exit() };
        match exited {
            Ok(outer_state) => drop(outer_state),
            Err(error) => {
                NoPreempt::release(());
                panic!("failed to leave the per-CPU IRQ guard: {error:?}");
            }
        }
        NoPreempt::release(());
    }
}

/// A nestable IRQ-save guard shared with the IRQ-safe lock aliases in this crate.
///
/// Guards on one CPU may be dropped in any order. IRQs remain disabled until the last related
/// guard exits. The guard cannot move between CPUs because its state is stored per CPU.
#[must_use = "dropping the guard releases IRQ and preemption protection"]
pub struct NoPreemptIrqSave {
    /// Prevents moving this CPU-local guard to another thread or CPU.
    _local: PhantomData<*mut ()>,
}

/// Construction of a shared IRQ-save guard.
impl NoPreemptIrqSave {
    /// Enters the current CPU's shared IRQ-safe critical section.
    pub fn new() -> Self {
        NoIrqGuardPolicy::acquire();
        Self {
            _local: PhantomData,
        }
    }
}

/// Default construction of a nested IRQ-save guard.
impl Default for NoPreemptIrqSave {
    /// Enters the current CPU's shared IRQ-safe critical section.
    fn default() -> Self {
        Self::new()
    }
}

/// Releases one share of the current CPU's IRQ-safe critical section.
impl Drop for NoPreemptIrqSave {
    /// Restores IRQ state only when this is the final active guard on the CPU.
    fn drop(&mut self) {
        NoIrqGuardPolicy::release(());
    }
}

/// Returns the current CPU's IRQ nesting depth for integration tests.
#[cfg(test)]
pub(crate) fn current_depth() -> usize {
    // SAFETY: Test access runs on the current host or kernel CPU. The test owns this per-CPU
    // state and does not concurrently mutate it from another execution context.
    unsafe { IRQ_NESTING.current_ref_raw().depth() }
}

#[cfg(test)]
mod tests;
