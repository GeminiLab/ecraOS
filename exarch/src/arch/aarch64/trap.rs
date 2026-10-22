//! AArch64 EL1 exception entry and semantic interrupt decoding.
//!
//! GICv3 mechanisms complete hardware delivery before the common interrupt-exit safe point.

use core::arch::global_asm;

use aarch64_cpu::registers::{DAIF, ReadWriteable, Readable, VBAR_EL1, Writeable};
use cfg_or_dummy::cfg_or_dummy;

use crate::trap::{
    Exception, Interrupt, LocalInterrupt, PageFaultFlags, RawTrap, Trap, TrapDisposition,
    TrapFrameAccess,
};

pub use super::gicv3::{
    Aarch64ExternalIrqConfig, ArchIrq, init_external_controller, prepare_irq, set_irq_enabled,
};
use super::{
    context::TrapFrame,
    gicv3::{self, GicInterrupt},
};

global_asm!(include_str!("trap.S"));

unsafe extern "C" {
    /// The aligned EL1 exception vector table.
    ///
    /// Architecture assembly defines this entry point for VBAR_EL1.
    fn aarch64_vector_base();
}

/// Installs the current PE's EL1 vectors and local controller interface.
///
/// The complete redistributor region is available during late platform initialization.
pub fn init_percpu() {
    VBAR_EL1.set(aarch64_vector_base as *const () as usize as u64);
    gicv3::init_percpu();
    aarch64_cpu::asm::barrier::isb(aarch64_cpu::asm::barrier::SY);
}

/// Enables maskable interrupts at EL1.
///
/// Boot calls this after installing common handlers and per-CPU consumers.
pub fn enable_local() {
    DAIF.modify(DAIF::I::CLEAR);
}

/// Disables maskable interrupts at EL1.
///
/// The saved exception-frame state independently controls trap return.
pub fn disable_local() {
    DAIF.modify(DAIF::I::SET);
}

/// Returns whether maskable interrupts are enabled at EL1.
///
/// Preemption-safe waits require this local delivery capability.
pub fn local_enabled() -> bool {
    DAIF.get() & (1 << 7) == 0
}

/// Dispatches one acknowledged GIC interrupt and completes it before the safe point.
///
/// SGI, timer PPI and SPI namespaces remain distinct and the IPI path acquires no controller lock.
#[cfg_or_dummy(target_arch = "aarch64")]
pub fn handle_external(frame: &mut TrapFrame) -> TrapDisposition {
    let intid = gicv3::acknowledge();
    let disposition = match gicv3::decode_intid(intid) {
        GicInterrupt::Software => crate::trap::handle(
            frame,
            Trap::Interrupt(Interrupt::Local(LocalInterrupt::Software)),
        ),
        GicInterrupt::Timer => crate::trap::handle(
            frame,
            Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)),
        ),
        GicInterrupt::Spi(source) => {
            let irq = ArchIrq::new(source);
            let result = crate::trap::handle(frame, Trap::Interrupt(Interrupt::Global(irq)));
            if result == TrapDisposition::Unhandled {
                _ = set_irq_enabled(irq, false);
            }
            result
        }
        GicInterrupt::Special => return TrapDisposition::Handled,
        GicInterrupt::Unknown => TrapDisposition::Unhandled,
    };
    gicv3::end_of_interrupt(intid);
    crate::trap::on_interrupt_exit(frame);
    disposition
}

/// Decodes one synchronous AArch64 exception and dispatches it to common handlers.
#[cfg_or_dummy(target_arch = "aarch64")]
#[unsafe(no_mangle)]
extern "C" fn aarch64_trap_handler(frame: &mut TrapFrame) {
    let ec = (frame.esr_el1 >> 26) & 0x3f;
    let is_irq = frame.vector % 4 == 1;
    let disposition = if is_irq {
        handle_external(frame)
    } else {
        let trap = match ec {
            0x20 | 0x21 => Trap::Exception(Exception::Unknown(RawTrap(frame.esr_el1))),
            0x24 | 0x25 => Trap::Exception(Exception::PageFault {
                address: memory_addr::va!(frame.far_el1),
                flags: PageFaultFlags::READ,
                is_user: frame.is_user(),
            }),
            0x3c => Trap::Exception(Exception::Breakpoint),
            _ => Trap::Exception(Exception::Unknown(RawTrap(frame.esr_el1))),
        };
        crate::trap::handle(frame, trap)
    };
    if disposition == TrapDisposition::Unhandled {
        log::warn!("Unhandled AArch64 trap: esr={:#x}", frame.esr_el1);
    }
}
