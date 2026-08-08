//! Periodic kernel timer events.
//!
//! The kernel timer uses per-CPU absolute one-shot deadlines to provide periodic timer events.

use core::time::Duration;

use exarch::trap::{Handler, LocalInterrupt, TrapDisposition, TrapFrame};
use expercpu::def_percpu;

mod deadline;

use deadline::select_next_deadline;

/// The number of periodic timer events requested each second.
///
/// This is the selected kernel timer-event cadence in hertz.
pub const TIMER_EVENT_HZ: u64 = 100;

/// The maximum allowed lateness for a timer deadline smoke test.
///
/// A deadline that fires later than this interval fails the boot smoke test.
const TIMER_DEADLINE_TOLERANCE: Duration = Duration::from_millis(20);

/// The interval between periodic timer-event deadlines in nanoseconds.
///
/// This is exact because one second is divisible by [`TIMER_EVENT_HZ`].
const PERIODIC_INTERVAL_NANOS: u64 = 1_000_000_000 / TIMER_EVENT_HZ;

/// The next deadline to program on the current hart.
///
/// Zero means this hart has not yet seeded its periodic schedule.
#[def_percpu]
static NEXT_DEADLINE_NANOS: u64 = 0;

/// The number of periodic timer events handled on the current hart.
///
/// Local interrupt exclusion serializes updates on each hart.
#[def_percpu]
static TIMER_EVENT_COUNT: u64 = 0;

/// Initializes the periodic timer on the bootstrap hart.
///
/// This registers the shared handler before arming and enabling local interrupts.
pub fn init_bsp() {
    assert_eq!(
        crate::irq::register(Handler::LocalInterrupt(handle_local_interrupt)),
        Ok(()),
        "failed to register the local timer handler"
    );
    program_next_timer();
    exarch::irq::enable_local();
}

/// Converts the semantic local timer trap into the periodic timer callback.
fn handle_local_interrupt(_frame: &mut TrapFrame, interrupt: LocalInterrupt) -> TrapDisposition {
    match interrupt {
        LocalInterrupt::Timer => {
            handle_timer_irq();
            TrapDisposition::Handled
        }
        LocalInterrupt::Software => TrapDisposition::Unhandled,
    }
}

/// Initializes the periodic timer on an application hart.
///
/// The bootstrap hart has already installed the shared timer handler.
pub fn init_ap() {
    program_next_timer();
    exarch::irq::enable_local();
}

/// Returns the timer-event count for the current hart.
///
/// The count is modified only while local interrupts are disabled.
pub fn timer_event_count() -> u64 {
    TIMER_EVENT_COUNT.read_current()
}

/// Verifies that timer interrupts advance on the current hart.
///
/// This checks delivery without asserting an exact rate that would be sensitive to host pauses.
pub fn smoke_test() {
    let mut observed = timer_event_count();
    for step in 1..=3 {
        let deadline = exarch::time::monotonic_time() + Duration::from_millis(10);
        exarch::time::set_oneshot_timer(deadline);
        let timeout = deadline + TIMER_DEADLINE_TOLERANCE;
        while timer_event_count() <= observed && exarch::time::monotonic_time() < timeout {
            core::hint::spin_loop();
        }
        let after = timer_event_count();
        let actual = exarch::time::monotonic_time();
        assert!(
            after > observed,
            "timer deadline {step} did not fire on CPU {}: deadline={deadline:?}, actual={actual:?}, timeout={timeout:?}",
            crate::mp::current_cpu_phys_id()
        );
        assert!(
            actual >= deadline,
            "timer deadline {step} fired early on CPU {}: deadline={deadline:?}, actual={actual:?}",
            crate::mp::current_cpu_phys_id()
        );
        assert!(
            actual <= timeout,
            "timer deadline {step} fired late on CPU {}: deadline={deadline:?}, actual={actual:?}, tolerance={TIMER_DEADLINE_TOLERANCE:?}",
            crate::mp::current_cpu_phys_id()
        );
        log::debug!(
            "Timer deadline {step} on CPU {}: deadline={deadline:?}, actual={actual:?}, lateness={:?}",
            crate::mp::current_cpu_phys_id(),
            actual - deadline
        );
        observed = after;
    }
    log::info!(
        "Timer deadline smoke test on CPU {}: 3 deadlines, {} events",
        crate::mp::current_cpu_phys_id(),
        observed
    );
}

/// Handles a supervisor timer interrupt on the current hart.
///
/// The handler advances the periodic deadline and programs the next SBI event.
fn handle_timer_irq() {
    TIMER_EVENT_COUNT.write_current(TIMER_EVENT_COUNT.read_current().wrapping_add(1));
    program_next_timer();
}

/// Programs the next periodic deadline for the current hart.
///
/// Expired schedules restart one interval after the current monotonic time.
fn program_next_timer() {
    let now = exarch::time::monotonic_time();
    let now_ns = u64::try_from(now.as_nanos()).expect("monotonic time exceeds u64 nanoseconds");
    let scheduled_ns = NEXT_DEADLINE_NANOS.read_current();
    let (deadline_ns, following_ns) =
        select_next_deadline(now_ns, scheduled_ns, PERIODIC_INTERVAL_NANOS);
    NEXT_DEADLINE_NANOS.write_current(following_ns);
    exarch::time::set_oneshot_timer(Duration::from_nanos(deadline_ns));
}
