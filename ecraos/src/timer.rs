//! Periodic kernel timer events.
//!
//! The RISC-V timer uses per-hart SBI one-shot deadlines to provide 100 timer events per second.

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

/// The number of periodic timer events handled on the current hart.
///
/// Local interrupt exclusion serializes updates on each hart.
#[def_percpu]
static TIMER_EVENT_COUNT: u64 = 0;

/// The kernel implementation of architecture trap callbacks.
///
/// This currently forwards IRQs to the architecture registry. The RISC-V trap path could call
/// `exarch::irq::handle` directly, so this crate-interface layer is not mechanically required and
/// remains only to preserve the existing common trap boundary for now.
struct TrapHandlerImpl;

#[crate_interface::impl_interface]
impl exarch::trap::TrapHandler for TrapHandlerImpl {
    fn handle_irq(irq: usize) -> bool {
        exarch::irq::handle(irq)
    }
}

/// Initializes the periodic timer on the bootstrap hart.
///
/// This registers the shared handler before arming and enabling local interrupts.
pub fn init_bsp() {
    assert!(
        exarch::irq::register(exarch::irq::TIMER_IRQ_NUM, handle_timer_irq),
        "failed to register the RISC-V timer IRQ handler"
    );
    program_next_timer();
    exarch::irq::enable_local();
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
    let before = timer_event_count();
    exarch::time::spin_wait_for(Duration::from_secs(1));
    let after = timer_event_count();
    assert!(
        after > before,
        "timer IRQ did not advance on hart {}",
        crate::mp::current_cpu_phys_id()
    );
    log::info!(
        "Timer event smoke test on hart {}: {} events",
        crate::mp::current_cpu_phys_id(),
        after - before
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
