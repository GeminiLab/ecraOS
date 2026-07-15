//! Advanced Programmable Interrupt Controller (APIC) support.

use core::mem::MaybeUninit;

use log::info;
use x2apic::lapic::{LocalApic, LocalApicBuilder};
use x86_64::instructions::port::Port;

use self::vectors::*;

pub(super) mod vectors {
    pub const APIC_TIMER_VECTOR: u8 = 0xf0;
    pub const APIC_SPURIOUS_VECTOR: u8 = 0xf1;
    pub const APIC_ERROR_VECTOR: u8 = 0xf2;
}

// const IO_APIC_BASE: PhysAddr = pa!(0xFEC0_0000);

static mut LOCAL_APIC: MaybeUninit<LocalApic> = MaybeUninit::uninit();
static mut IS_X2APIC: bool = false;
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

#[allow(static_mut_refs)]
pub fn local_apic<'a>() -> &'a mut LocalApic {
    // It's safe as `LOCAL_APIC` is initialized in `init_primary`.
    unsafe { LOCAL_APIC.assume_init_mut() }
}

#[cfg(false)]
pub fn raw_apic_id(id_u8: u8) -> u32 {
    if unsafe { IS_X2APIC } {
        id_u8 as u32
    } else {
        (id_u8 as u32) << 24
    }
}

fn cpu_has_x2apic() -> bool {
    match raw_cpuid::CpuId::new().get_feature_info() {
        Some(finfo) => finfo.has_x2apic(),
        None => false,
    }
}

pub fn init_primary() {
    info!("Initialize Local APIC...");

    unsafe {
        // Disable 8259A interrupt controllers
        Port::<u8>::new(0x21).write(0xff);
        Port::<u8>::new(0xA1).write(0xff);
    }

    let mut builder = LocalApicBuilder::new();
    builder
        .timer_vector(APIC_TIMER_VECTOR as _)
        .error_vector(APIC_ERROR_VECTOR as _)
        .spurious_vector(APIC_SPURIOUS_VECTOR as _);

    if cpu_has_x2apic() {
        info!("Using x2APIC.");
        unsafe { IS_X2APIC = true };
    } else {
        // info!("Using xAPIC.");
        // let base_vaddr = phys_to_virt(pa!(unsafe { xapic_base() } as usize));
        // builder.set_xapic_base(base_vaddr.as_usize() as u64);
        panic!("CPU does not support x2APIC.")
    }

    let mut lapic = builder.build().unwrap();
    unsafe {
        lapic.enable();
        #[allow(static_mut_refs)]
        LOCAL_APIC.write(lapic);
    }

    // info!("Initialize IO APIC...");
    // let io_apic = unsafe { IoApic::new(phys_to_virt(IO_APIC_BASE).as_usize() as u64) };
    // IO_APIC.init_once(SpinNoIrq::new(io_apic));
}
