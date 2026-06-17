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
}
