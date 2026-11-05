//! Hardware IRQ-state checks for shared spin-lock and reader-writer guards.

use ecraos::{mp, reexport::exarch, task};
use exspin::{NoPreemptIrqSave, RwLockNoIrq, SpinNoIrq};
use log::info;

/// Verifies both initial IRQ states and non-LIFO guard release on the current CPU.
pub(super) fn check() {
    assert!(exarch::trap::local_enabled());
    for initially_enabled in [false, true] {
        if !initially_enabled {
            exarch::trap::disable_local();
        }
        let spin = SpinNoIrq::new(1);
        let a = RwLockNoIrq::new(2);
        let b = RwLockNoIrq::new(3);
        let execution = NoPreemptIrqSave::new();
        let first = spin.lock();
        let second = a.read();
        let third = b.write();
        assert!(!exarch::trap::local_enabled());
        assert!(!task::can_schedule_now());
        drop(execution);
        drop(first);
        drop(second);
        assert!(!exarch::trap::local_enabled());
        assert!(b.try_read().is_none());
        assert!(b.try_write().is_none());
        assert!(!exarch::trap::local_enabled());
        drop(third);
        assert_eq!(exarch::trap::local_enabled(), initially_enabled);
        if !initially_enabled {
            exarch::trap::enable_local();
        }
        assert!(task::can_schedule_now());
    }
    info!(
        "IRQ nesting and non-LIFO release passed on CPU {}",
        mp::current_cpu_id()
    );
}
