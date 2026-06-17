//! Time related utilities.

pub use core::time::Duration;

use crate_interface::def_interface;

/// A measurement of the system clock.
///
/// Currently, it reuses the [`core::time::Duration`] type. But it does not
/// represent a duration, but a clock time.
pub type TimeValue = Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Ticks(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub u64);

impl Nanos {
    pub const fn to_time_value(self) -> TimeValue {
        TimeValue::from_nanos(self.0)
    }
}

#[def_interface(gen_caller)]
pub trait TimeIf {
    fn monotonic_ticks() -> Ticks;

    fn ticks_to_nanos(ticks: Ticks) -> Nanos;

    fn nanos_to_ticks(nanos: Nanos) -> Ticks;
}

pub fn monotonic_time() -> TimeValue {
    ticks_to_nanos(monotonic_ticks()).to_time_value()
}

pub fn spin_wait_for(duration: TimeValue) {
    spin_wait_until(monotonic_time() + duration);
}

pub fn spin_wait_until(time: TimeValue) {
    while monotonic_time() < time {
        core::hint::spin_loop();
    }
}
