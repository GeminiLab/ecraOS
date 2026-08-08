use core::arch::x86_64::_rdtsc;

use crate::{
    dbcn_println,
    time::{Nanos, Ticks, TimeIf},
};

mod calibration;

/// Calibrates the LAPIC one-shot countdown against the PIT reference clock.
///
/// The returned value is the LAPIC countdown frequency in kHz.
pub fn calibrate_lapic_timer() -> Option<u64> {
    calibration::calibrate_lapic_with_pit()
}

static mut INIT_TICK: Ticks = Ticks(0);
static mut TSC_FREQ_KHZ: u64 = 0;

/// TSC frequency used when calibration fails. It's almost definitely wrong, but it should be enough
/// for the system to boot and enumerate other time devices.
const TSC_FREQ_KHZ_BLIND: u64 = 2_000_000;

fn current_ticks() -> Ticks {
    unsafe { Ticks(_rdtsc()) }
}

/// Converts nanoseconds to LAPIC countdown ticks.
///
/// The result is clamped to the nonzero range accepted by the 32-bit initial-count register.
fn nanos_to_lapic_ticks(nanos: u64, frequency_khz: u64) -> u32 {
    let ticks = nanos.saturating_mul(frequency_khz) / 1_000_000;
    ticks.clamp(1, u32::MAX as u64) as u32
}

pub fn init_early() {
    unsafe {
        INIT_TICK = current_ticks();
        TSC_FREQ_KHZ = calibration::calibrate_tsc_early().unwrap_or_else(|| {
            dbcn_println!(
                "TSC calibration failed, using blind value: {} kHz",
                TSC_FREQ_KHZ_BLIND
            );
            TSC_FREQ_KHZ_BLIND
        });
    }
}

pub struct TimeImpl;

#[crate_interface::impl_interface]
impl TimeIf for TimeImpl {
    fn monotonic_ticks() -> Ticks {
        unsafe { Ticks(current_ticks().0 - INIT_TICK.0) }
    }

    fn ticks_to_nanos(ticks: Ticks) -> Nanos {
        unsafe { Nanos(ticks.0 * 1_000_000 / TSC_FREQ_KHZ) }
    }

    fn nanos_to_ticks(nanos: Nanos) -> Ticks {
        unsafe { Ticks(nanos.0 * TSC_FREQ_KHZ / 1_000_000) }
    }

    fn set_oneshot_timer(deadline: crate::time::TimeValue) {
        let deadline_ns =
            u64::try_from(deadline.as_nanos()).expect("x86 timer deadline exceeds u64 nanoseconds");
        if super::imp::apic::tsc_deadline_supported() {
            let deadline_ticks = unsafe { INIT_TICK.0 }
                .checked_add(Self::nanos_to_ticks(Nanos(deadline_ns)).0)
                .expect("x86 timer deadline overflows TSC");
            super::imp::apic::program_tsc_deadline(deadline_ticks.max(current_ticks().0 + 1));
            return;
        }
        let now_ns = Self::ticks_to_nanos(Self::monotonic_ticks()).0;
        let delta_ns = deadline_ns.saturating_sub(now_ns);
        let count = nanos_to_lapic_ticks(delta_ns, super::imp::apic::timer_frequency_khz());
        super::imp::apic::program_timer_initial(count);
    }
}

#[cfg(test)]
mod tests {
    use super::nanos_to_lapic_ticks;

    /// Verifies conversion in the LAPIC timer frequency domain.
    #[test]
    fn converts_nanoseconds_in_lapic_frequency_domain() {
        assert_eq!(nanos_to_lapic_ticks(10_000_000, 1_000_000), 10_000_000);
        assert_eq!(nanos_to_lapic_ticks(10_000_000, 3_500_000), 35_000_000);
    }

    /// Verifies clamping to the LAPIC initial-count register range.
    #[test]
    fn clamps_lapic_timer_count_to_hardware_range() {
        assert_eq!(nanos_to_lapic_ticks(0, 1_000_000), 1);
        assert_eq!(nanos_to_lapic_ticks(u64::MAX, u64::MAX), u32::MAX);
    }
}
