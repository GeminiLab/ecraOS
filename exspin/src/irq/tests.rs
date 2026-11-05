//! Tests for nested interrupt-state lifetime management.

use core::cell::Cell;

use super::NestedIrqState;

thread_local! {
    /// Simulated local IRQ enable state for the current host thread.
    static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
}

/// A saved local interrupt state that restores its value when the outer token is dropped.
#[derive(Debug)]
pub(super) struct SimulatedIrqSave(bool);

impl SimulatedIrqSave {
    /// Disables interrupts and saves their previous state.
    pub(super) fn new() -> Self {
        Self(IRQ_ENABLED.with(|enabled| enabled.replace(false)))
    }
}

/// Confirms that mixed lock guards defer simulated IRQ restoration despite non-LIFO drops.
#[cfg(feature = "host-test")]
#[test]
fn mixed_lock_guards_preserve_irqs_until_the_last_guard() {
    use crate::{NoPreemptIrqSave, RwLockNoIrq, SpinNoIrq};

    let spin = SpinNoIrq::new(1);
    let rwlock = RwLockNoIrq::new(2);
    let direct = NoPreemptIrqSave::new();
    let first = spin.lock();
    let second = rwlock.read();
    assert_eq!(super::current_depth(), 3);
    drop(direct);
    drop(first);
    assert!(!IRQ_ENABLED.with(Cell::get));
    assert_eq!(super::current_depth(), 1);
    drop(second);
    assert!(IRQ_ENABLED.with(Cell::get));
    assert_eq!(super::current_depth(), 0);
}

/// Keeps the original IRQ state across overlapping read/write guards in either drop order.
#[cfg(feature = "host-test")]
#[test]
fn rwlock_guards_preserve_initial_irqs_across_release_orders() {
    use crate::RwLockNoIrq;

    for initially_enabled in [false, true] {
        for first_write in [false, true] {
            for second_write in [false, true] {
                for first_dropped_first in [false, true] {
                    IRQ_ENABLED.with(|enabled| enabled.set(initially_enabled));
                    let a = RwLockNoIrq::new(1);
                    let b = RwLockNoIrq::new(2);
                    let first_read = (!first_write).then(|| a.read());
                    let first_writer = first_write.then(|| a.write());
                    let second_read = (!second_write).then(|| b.read());
                    let second_writer = second_write.then(|| b.write());
                    assert_eq!(super::current_depth(), 2);
                    if first_dropped_first {
                        drop((first_read, first_writer));
                        assert!(!IRQ_ENABLED.with(Cell::get));
                        drop((second_read, second_writer));
                    } else {
                        drop((second_read, second_writer));
                        assert!(!IRQ_ENABLED.with(Cell::get));
                        drop((first_read, first_writer));
                    }
                    assert_eq!(IRQ_ENABLED.with(Cell::get), initially_enabled);
                    assert_eq!(super::current_depth(), 0);
                }
            }
        }
    }
    IRQ_ENABLED.with(|enabled| enabled.set(true));
}

/// Rolls back failed lock attempts without restoring IRQs held disabled by another guard.
#[cfg(feature = "host-test")]
#[test]
fn failed_lock_attempts_preserve_active_irq_protection() {
    use crate::{RwLockNoIrq, SpinNoIrq};

    let spin = SpinNoIrq::new(1);
    let rwlock = RwLockNoIrq::new(2);
    let first = spin.lock();
    let writer = rwlock.write();
    assert!(spin.try_lock().is_none());
    assert!(rwlock.try_read().is_none());
    assert!(rwlock.try_write().is_none());
    assert_eq!(super::current_depth(), 2);
    assert!(!IRQ_ENABLED.with(Cell::get));
    drop(first);
    assert!(!IRQ_ENABLED.with(Cell::get));
    drop(writer);
    assert!(IRQ_ENABLED.with(Cell::get));
    assert_eq!(super::current_depth(), 0);
}

/// Releases the outer saved IRQ state when unwinding overlapping lock guards.
#[cfg(feature = "host-test")]
#[test]
fn unwinding_releases_all_irq_safe_guards() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::{RwLockNoIrq, SpinNoIrq};

    let spin = SpinNoIrq::new(1);
    let rwlock = RwLockNoIrq::new(2);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let first = spin.lock();
        let _second = rwlock.write();
        drop(first);
        assert!(!IRQ_ENABLED.with(Cell::get));
        panic!("unwind the remaining IRQ-safe lock");
    }));
    assert!(result.is_err());
    assert!(IRQ_ENABLED.with(Cell::get));
    assert_eq!(super::current_depth(), 0);
    assert!(spin.try_lock().is_some());
    assert!(rwlock.try_write().is_some());
}

impl Drop for SimulatedIrqSave {
    /// Restores the state saved by the outermost nested section.
    fn drop(&mut self) {
        IRQ_ENABLED.with(|enabled| enabled.set(self.0));
    }
}

/// Keeps IRQs disabled until all guards leave, regardless of their drop order.
#[test]
fn out_of_order_exit_restores_only_after_the_last_guard() {
    let mut nesting = NestedIrqState::new();
    assert!(nesting.enter(SimulatedIrqSave::new()).unwrap().is_none());
    let inner = nesting.enter(SimulatedIrqSave::new()).unwrap();
    assert!(inner.is_some());
    assert!(!IRQ_ENABLED.with(Cell::get));

    drop(inner);
    drop(nesting.exit().unwrap());
    assert!(!IRQ_ENABLED.with(Cell::get));
    drop(nesting.exit().unwrap());
    assert!(IRQ_ENABLED.with(Cell::get));
}

/// Preserves an initially disabled IRQ state through nested sections.
#[test]
fn outermost_exit_preserves_previously_disabled_irqs() {
    IRQ_ENABLED.with(|enabled| enabled.set(false));
    let mut nesting = NestedIrqState::new();
    assert!(nesting.enter(SimulatedIrqSave::new()).unwrap().is_none());
    let inner = nesting.enter(SimulatedIrqSave::new()).unwrap();
    drop(inner);
    drop(nesting.exit().unwrap());
    drop(nesting.exit().unwrap());
    assert!(!IRQ_ENABLED.with(Cell::get));

    IRQ_ENABLED.with(|enabled| enabled.set(true));
}

/// Returns the temporary IRQ token when nesting depth cannot be incremented.
#[test]
fn depth_overflow_preserves_the_outer_irq_state() {
    IRQ_ENABLED.with(|enabled| enabled.set(true));
    let mut nesting = NestedIrqState::new();
    assert!(nesting.enter(SimulatedIrqSave::new()).unwrap().is_none());
    nesting.depth = usize::MAX;

    let (error, temporary) = nesting.enter(SimulatedIrqSave::new()).unwrap_err();
    assert_eq!(error, super::NestedIrqError::DepthOverflow);
    assert_eq!(nesting.depth, usize::MAX);
    drop(temporary);
    assert!(!IRQ_ENABLED.with(Cell::get));

    // Restore the injected invalid depth so the test can release its outer token normally.
    nesting.depth = 1;
    drop(nesting.exit().unwrap());
    assert!(IRQ_ENABLED.with(Cell::get));
}
