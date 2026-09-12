//! AArch64 generic virtual timer support.

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTV_CTL_EL0, CNTV_CVAL_EL0, CNTVCT_EL0};
use aarch64_cpu::registers::{Readable, Writeable};

use crate::time::{Nanos, Ticks, TimeValue};

static mut INIT_TICK: u64 = 0;
static mut FREQUENCY_HZ: u64 = 1_000_000;

#[inline]
fn read_counter() -> u64 {
    CNTVCT_EL0.get()
}

#[inline]
fn read_frequency() -> u64 {
    CNTFRQ_EL0.get().max(1)
}

/// Initializes the generic timer frequency and monotonic origin.
pub fn init() {
    unsafe {
        FREQUENCY_HZ = read_frequency();
        INIT_TICK = read_counter();
    }
}

/// Returns monotonic virtual-counter ticks since early initialization.
pub fn monotonic_ticks() -> Ticks {
    let origin = unsafe { INIT_TICK };
    Ticks(read_counter().wrapping_sub(origin))
}

/// Converts generic timer ticks to nanoseconds.
pub fn ticks_to_nanos(ticks: Ticks) -> Nanos {
    let frequency = unsafe { FREQUENCY_HZ };
    Nanos((ticks.0 as u128 * 1_000_000_000 / frequency as u128) as u64)
}

/// Converts nanoseconds to generic timer ticks.
pub fn nanos_to_ticks(nanos: Nanos) -> Ticks {
    let frequency = unsafe { FREQUENCY_HZ };
    Ticks((nanos.0 as u128 * frequency as u128 / 1_000_000_000) as u64)
}

/// Programs the virtual timer for an absolute monotonic deadline.
pub fn set_oneshot_timer(deadline: TimeValue) {
    let deadline_ticks = nanos_to_ticks(Nanos(deadline.as_nanos() as u64));
    let origin = unsafe { INIT_TICK };
    let value = origin
        .wrapping_add(deadline_ticks.0)
        .max(read_counter().wrapping_add(1));
    CNTV_CVAL_EL0.set(value);
    CNTV_CTL_EL0.set(1);
    aarch64_cpu::asm::barrier::isb(aarch64_cpu::asm::barrier::SY);
}

#[cfg(test)]
mod tests {
    use super::{Nanos, Ticks};

    /// Verifies the timer unit conversion at a one-gigahertz frequency.
    #[test]
    fn timer_units_are_monotonic() {
        assert_eq!(Nanos(1_000_000_000).0, Ticks(1_000_000_000).0);
    }
}
