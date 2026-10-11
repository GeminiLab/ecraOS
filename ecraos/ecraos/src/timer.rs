//! Periodic kernel timer events.
//!
//! The kernel timer uses per-CPU absolute one-shot deadlines to provide periodic timer events.

use core::time::Duration;

use expercpu::def_percpu;

mod deadline;

use deadline::select_next_deadline;

/// The number of periodic timer events requested each second.
///
/// This is the selected kernel timer-event cadence in hertz.
pub const TIMER_EVENT_HZ: u64 = 100;

/// The interval between periodic timer-event deadlines in nanoseconds.
///
/// This is exact because one second is divisible by [`TIMER_EVENT_HZ`].
const PERIODIC_INTERVAL_NANOS: u64 = 1_000_000_000 / TIMER_EVENT_HZ;

/// The next deadline to program on the current hart.
///
/// Zero means this hart has not yet seeded its periodic schedule.
#[def_percpu]
static NEXT_DEADLINE_NANOS: u64 = 0;

/// Initializes the periodic timer on the bootstrap hart.
///
/// This registers the shared handler before arming and enabling local interrupts.
pub fn init_bsp() {
    program_next_timer();
    exarch::trap::enable_local();
}

/// Initializes the periodic timer on an application hart.
///
/// The bootstrap hart has already installed the shared timer handler.
pub fn init_ap() {
    program_next_timer();
    exarch::trap::enable_local();
}

/// Handles a supervisor timer interrupt on the current hart.
///
/// The handler advances the periodic deadline and programs the next SBI event.
pub(crate) fn timer_tick() {
    crate::task::wake_sleepers(exarch::time::monotonic_time());
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
