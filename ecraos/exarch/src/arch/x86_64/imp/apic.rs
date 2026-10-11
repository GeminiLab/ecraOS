//! Advanced Programmable Interrupt Controller (APIC) support.

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};

use expercpu::def_percpu;
use log::info;
use x2apic::lapic::{LocalApic, LocalApicBuilder, TimerDivide, TimerMode};
use x86_64::instructions::port::Port;

use self::vectors::*;

pub(super) mod vectors {
    pub const APIC_TIMER_VECTOR: u8 = 0xf0;
    pub const APIC_SPURIOUS_VECTOR: u8 = 0xf1;
    pub const APIC_ERROR_VECTOR: u8 = 0xf2;
}

// const IO_APIC_BASE: PhysAddr = pa!(0xFEC0_0000);

#[def_percpu]
static LOCAL_APIC: usize = 0;
static TSC_DEADLINE_SUPPORTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// The calibrated LAPIC countdown frequency in kHz.
///
/// The BSP publishes the value for application processors to reuse.
static LAPIC_TIMER_FREQ_KHZ: AtomicU64 = AtomicU64::new(0);
// static IO_APIC: LazyInit<SpinNoIrq<IoApic>> = LazyInit::new();

/// Enables or disables the given IRQ.
// pub fn set_enable(vector: usize, enabled: bool) {
//     // should not affect LAPIC interrupts
//     if vector < APIC_TIMER_VECTOR as _ {
//         unsafe {
//             if enabled {
//                 IO_APIC.lock().enable_irq(vector as u8);
//             } else {
//                 IO_APIC.lock().disable_irq(vector as u8);
//             }
//         }
//     }
// }

/// Invokes a callback with the current CPU's initialized local APIC.
///
/// This panics when local APIC initialization has not installed the current CPU's pointer.
pub fn with_local_apic<R>(f: impl FnOnce(&mut LocalApic) -> R) -> R {
    let local = LOCAL_APIC.read_current();
    assert_ne!(local, 0, "current CPU local APIC is not initialized");
    // SAFETY: The pointer is installed once by the current CPU and remains valid forever.
    f(unsafe { &mut *(local as *mut LocalApic) })
}

/// Sends an end-of-interrupt notification for the current local APIC.
pub fn end_of_interrupt() {
    // SAFETY: The local APIC is initialized before external vectors are enabled.
    with_local_apic(|lapic| unsafe { lapic.end_of_interrupt() });
}

#[cfg(false)]
pub fn raw_apic_id(id_u8: u8) -> u32 {
    id_u8 as u32
}

fn cpu_has_x2apic() -> bool {
    match raw_cpuid::CpuId::new().get_feature_info() {
        Some(finfo) => finfo.has_x2apic(),
        None => false,
    }
}

pub fn init_bsp() {
    info!("Initialize Local APIC...");

    unsafe {
        // Disable 8259A interrupt controllers
        Port::<u8>::new(0x21).write(0xff);
        Port::<u8>::new(0xA1).write(0xff);
    }

    init_local_apic();
}

/// Initializes the local APIC on an application CPU.
pub fn init_ap() {
    init_local_apic();
}

fn init_local_apic() {
    let has_tsc_deadline = raw_cpuid::CpuId::new()
        .get_feature_info()
        .is_some_and(|info| info.has_tsc_deadline());
    TSC_DEADLINE_SUPPORTED.store(has_tsc_deadline, Ordering::Release);
    let mut builder = LocalApicBuilder::new();
    builder
        .timer_vector(APIC_TIMER_VECTOR as _)
        .timer_divide(TimerDivide::Div1)
        .timer_mode(if has_tsc_deadline {
            TimerMode::TscDeadline
        } else {
            TimerMode::OneShot
        })
        .error_vector(APIC_ERROR_VECTOR as _)
        .spurious_vector(APIC_SPURIOUS_VECTOR as _);

    if cpu_has_x2apic() {
        // x2APIC support was checked before entering this path.
    } else {
        // info!("Using xAPIC.");
        // let base_vaddr = phys_to_virt(pa!(unsafe { xapic_base() } as usize));
        // builder.set_xapic_base(base_vaddr.as_usize() as u64);
        panic!("CPU does not support x2APIC.")
    }

    let mut lapic = builder.build().unwrap();
    unsafe {
        if has_tsc_deadline {
            x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, 0);
        }
        lapic.enable();
        lapic.disable_timer();
        lapic.set_timer_initial(0);
    }
    let lapic = Box::leak(Box::new(lapic)) as *mut LocalApic as usize;
    LOCAL_APIC.write_current(lapic);
    if !has_tsc_deadline && LAPIC_TIMER_FREQ_KHZ.load(Ordering::Acquire) == 0 {
        let frequency = crate::arch::x86_64::time::calibrate_lapic_timer()
            .expect("failed to calibrate LAPIC timer");
        LAPIC_TIMER_FREQ_KHZ.store(frequency, Ordering::Release);
        info!("Calibrated LAPIC timer: {frequency} kHz");
    }
}

/// Returns whether the current CPU supports TSC-Deadline mode.
pub fn tsc_deadline_supported() -> bool {
    TSC_DEADLINE_SUPPORTED.load(Ordering::Acquire)
}

/// Returns the calibrated LAPIC timer frequency in kHz.
///
/// This rejects use of the one-shot timer before calibration has completed.
pub fn timer_frequency_khz() -> u64 {
    let frequency = LAPIC_TIMER_FREQ_KHZ.load(Ordering::Acquire);
    assert_ne!(frequency, 0, "LAPIC timer frequency is not calibrated");
    frequency
}

/// Programs the current LAPIC one-shot initial count.
pub fn program_timer_initial(count: u32) {
    with_local_apic(|lapic| unsafe {
        lapic.set_timer_initial(count);
        lapic.enable_timer();
    });
}

/// Programs an absolute TSC-Deadline value on the current CPU.
pub fn program_tsc_deadline(deadline: u64) {
    assert!(tsc_deadline_supported(), "TSC-Deadline mode is unavailable");
    with_local_apic(|lapic| unsafe {
        lapic.set_timer_initial(0);
        lapic.enable_timer();
    });
    unsafe { x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline) };
}

/// Sends an INIT IPI through the current CPU's local APIC.
pub unsafe fn send_init_ipi(destination: u32) {
    with_local_apic(|lapic| unsafe { lapic.send_init_ipi(destination) });
}

/// Sends a STARTUP IPI through the current CPU's local APIC.
pub unsafe fn send_sipi(vector: u8, destination: u32) {
    with_local_apic(|lapic| unsafe { lapic.send_sipi(vector, destination) });
}
