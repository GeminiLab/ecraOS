use fdt_rs::prelude::{FallibleIterator, PropReader};

use crate::time::{Nanos, Ticks, TimeIf, TimeValue};

fn current_ticks() -> Ticks {
    Ticks(riscv::register::time::read() as _)
}

static mut INIT_TICK: Ticks = Ticks(0);
static mut TIME_FREQ_KHZ: u64 = 0;

/// Time frequency used when calibration fails (10 MHz). It's almost definitely wrong, but it should
/// be enough for the system to boot and enumerate other time devices.
const TIME_FREQ_HZ_BLIND: u64 = 10_000_000;

pub struct TimeImpl;

#[crate_interface::impl_interface]
impl TimeIf for TimeImpl {
    fn monotonic_ticks() -> Ticks {
        unsafe { Ticks(current_ticks().0 - INIT_TICK.0) }
    }

    fn ticks_to_nanos(ticks: Ticks) -> Nanos {
        unsafe { Nanos(ticks.0 * 1_000_000 / TIME_FREQ_KHZ) }
    }

    fn nanos_to_ticks(nanos: Nanos) -> Ticks {
        unsafe { Ticks(nanos.0 * TIME_FREQ_KHZ / 1_000_000) }
    }

    fn set_oneshot_timer(deadline: TimeValue) {
        let deadline_ns = u64::try_from(deadline.as_nanos())
            .expect("timer deadline does not fit in the RISC-V time domain");
        let relative_ticks = Self::nanos_to_ticks(Nanos(deadline_ns));
        let absolute_ticks = unsafe {
            INIT_TICK
                .0
                .checked_add(relative_ticks.0)
                .expect("RISC-V timer deadline overflow")
        };
        sbi_rt::set_timer(absolute_ticks).expect("SBI TIME set_timer failed");
    }
}

pub fn init() {
    let dtb = super::DTB.get().expect("Device Tree not initialized");

    let cpus = dtb
        .nodes()
        .find(|n| n.name().map(|name| name == "cpus"))
        .ok()
        .flatten();
    let freq_prop = cpus.and_then(|cpus| {
        cpus.props()
            .find(|p| p.name().map(|name| name == "timebase-frequency"))
            .ok()
            .flatten()
    });
    let freq_hz = freq_prop
        .and_then(|p| p.u32(0).ok())
        .unwrap_or(TIME_FREQ_HZ_BLIND as _);

    unsafe {
        TIME_FREQ_KHZ = (freq_hz / 1_000) as _;
        INIT_TICK = current_ticks();
    }
}
