use core::arch::x86_64::_rdtsc;

use crate::{
    dbcn_println,
    time::{Nanos, Ticks, TimeIf},
};

mod calibration;

static mut INIT_TICK: Ticks = Ticks(0);
static mut TSC_FREQ_KHZ: u64 = 0;

/// TSC frequency used when calibration fails. It's almost definitely wrong, but it should be enough
/// for the system to boot and enumerate other time devices.
const TSC_FREQ_KHZ_BLIND: u64 = 2_000_000;

fn current_ticks() -> Ticks {
    unsafe { Ticks(_rdtsc()) }
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
        let deadline_ticks = unsafe { INIT_TICK.0 }
            .checked_add(
                Self::nanos_to_ticks(Nanos(
                    u64::try_from(deadline.as_nanos())
                        .expect("x86 timer deadline exceeds u64 nanoseconds"),
                ))
                .0,
            )
            .expect("x86 timer deadline overflows TSC");
        if super::imp::apic::tsc_deadline_supported() {
            super::imp::apic::program_tsc_deadline(deadline_ticks.max(current_ticks().0 + 1));
            return;
        }
        let now = Self::monotonic_ticks().0;
        let delta = deadline_ticks
            .saturating_sub(unsafe { INIT_TICK.0 + now })
            .max(1)
            .min(u32::MAX as u64) as u32;
        super::imp::apic::program_timer_initial(delta);
    }
}
