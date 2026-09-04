//! Trap handlers.
//!
//! Original code from `axcpu` v0.3.1.

use memory_addr::va;
use x86::{controlregs::cr2, irq::*};
use x86_64::structures::idt::PageFaultErrorCode;

use super::super::context::TrapFrame;
use crate::trap::{Exception, Interrupt, LocalInterrupt, PageFaultFlags, Trap, TrapDisposition};

core::arch::global_asm!(include_str!("trap.S"));

const IRQ_VECTOR_START: u8 = 0x20;
const IRQ_VECTOR_END: u8 = 0xff;

fn handle_page_fault(tf: &mut TrapFrame) {
    let access_flags = err_code_to_flags(tf.error_code)
        .unwrap_or_else(|e| panic!("Invalid #PF error code: {:#x}", e));
    let vaddr = va!(unsafe { cr2() });
    if !matches!(
        crate::trap::handle(
            tf,
            Trap::Exception(Exception::PageFault {
                address: vaddr,
                flags: access_flags,
                is_user: tf.is_user(),
            }),
        ),
        TrapDisposition::Handled
    ) {
        panic!(
            "Unhandled {} #PF @ {:#x}, fault_vaddr={:#x}, error_code={:#x} ({:?}):\n{:#x?}",
            if tf.is_user() { "user" } else { "kernel" },
            tf.rip,
            vaddr,
            tf.error_code,
            access_flags,
            tf,
        );
    }
}

#[unsafe(no_mangle)]
fn x86_trap_handler(tf: &mut TrapFrame) {
    match tf.vector as u8 {
        PAGE_FAULT_VECTOR => handle_page_fault(tf),
        BREAKPOINT_VECTOR => {
            _ = crate::trap::handle(tf, Trap::Exception(Exception::Breakpoint));
        }
        GENERAL_PROTECTION_FAULT_VECTOR => {
            _ = crate::trap::handle(
                tf,
                Trap::Exception(Exception::GeneralProtection {
                    error_code: tf.error_code as usize,
                }),
            );
        }
        IRQ_VECTOR_START..=IRQ_VECTOR_END => {
            let vector = tf.vector as u8;
            if vector == super::apic::vectors::APIC_TIMER_VECTOR {
                _ = crate::trap::handle(
                    tf,
                    Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)),
                );
                super::apic::end_of_interrupt();
            } else if let Some(irq) = crate::arch::x86_64::trap::gsi_for_vector(vector) {
                let result = crate::trap::handle(tf, Trap::Interrupt(Interrupt::Global(irq)));
                if matches!(result, TrapDisposition::Unhandled) {
                    crate::arch::x86_64::trap::mask_source(irq);
                }
                super::apic::end_of_interrupt();
            } else if vector == super::apic::vectors::APIC_SPURIOUS_VECTOR {
                log::warn!("spurious x86 interrupt vector {vector:#x}");
            } else {
                log::warn!("unknown x86 interrupt vector {vector:#x}");
                super::apic::end_of_interrupt();
            }
        }
        _ => {
            let exception = match tf.vector as u8 {
                INVALID_OPCODE_VECTOR => Exception::InvalidInstruction,
                _ => Exception::Unknown(crate::trap::RawTrap(tf.vector as usize)),
            };
            _ = crate::trap::handle(tf, Trap::Exception(exception));
        }
    }
}

fn err_code_to_flags(err_code: u64) -> Result<PageFaultFlags, u64> {
    let code = PageFaultErrorCode::from_bits_truncate(err_code);
    let reserved_bits = (PageFaultErrorCode::CAUSED_BY_WRITE
        | PageFaultErrorCode::USER_MODE
        | PageFaultErrorCode::INSTRUCTION_FETCH)
        .complement();
    if code.intersects(reserved_bits) {
        Err(err_code)
    } else {
        let mut flags = PageFaultFlags::empty();
        if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            flags |= PageFaultFlags::WRITE;
        } else {
            flags |= PageFaultFlags::READ;
        }
        if code.contains(PageFaultErrorCode::USER_MODE) {
            flags |= PageFaultFlags::USER;
        }
        if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
            flags |= PageFaultFlags::EXECUTE;
        }
        Ok(flags)
    }
}
