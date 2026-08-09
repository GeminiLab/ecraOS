//! Time related utilities.

pub use core::time::Duration;

use look_at::look_at;

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

/// Platform time operations.
///
/// The wrapper forwards calls to the selected architecture implementation.
#[look_at(crate::arch::current::time, flatten)]
mod _wrapper {
    /// Returns monotonic platform ticks since early initialization.
    pub fn monotonic_ticks() -> Ticks;

    /// Converts platform ticks to nanoseconds.
    pub fn ticks_to_nanos(ticks: Ticks) -> Nanos;

    /// Converts nanoseconds to platform ticks.
    pub fn nanos_to_ticks(nanos: Nanos) -> Ticks;

    /// Programs a one-shot timer for an absolute monotonic deadline.
    pub fn set_oneshot_timer(deadline: TimeValue);
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
