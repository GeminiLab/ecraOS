//! RISC-V supervisor trap handling.
//!
//! This module installs the direct trap vector and dispatches traps to common kernel callbacks.

use core::arch::global_asm;

use log::{error, warn};
use memory_addr::va;
use riscv::{
    interrupt::{
        Trap as RiscVTrap,
        supervisor::{self, Exception as RiscVException, Interrupt as RiscVInterrupt},
    },
    register::{
        scause::{self, Scause},
        sstatus, stval, stvec,
    },
};

use super::context::TrapFrame;
use crate::trap::{
    Exception, Interrupt, LocalInterrupt, PageFaultFlags, RawTrap, Trap, TrapDisposition,
};

global_asm!(
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
);

unsafe extern "C" {
    fn trap_vector_base();
}

/// Installs the supervisor trap vector for the current hart.
///
/// The caller must invoke [`after_reloc`] after code addresses change through relocation.
pub(super) fn init_percpu() {
    let vector = stvec::Stvec::new(
        trap_vector_base as *const () as usize,
        stvec::TrapMode::Direct,
    );
    // SAFETY: the assembly symbol is aligned and remains valid until the next relocation hook.
    unsafe { stvec::write(vector) };
    super::trap::init_percpu();
}

/// Reloads the supervisor trap vector after relocation.
///
/// The current hart retains its interrupt-source mask.
pub(super) fn after_reloc() {
    let vector = stvec::Stvec::new(
        trap_vector_base as *const () as usize,
        stvec::TrapMode::Direct,
    );
    // SAFETY: the relocated assembly symbol is the active kernel trap entry.
    unsafe { stvec::write(vector) };
}

/// Returns whether a trap interrupted user mode.
///
/// The saved previous privilege field describes the mode active before trap entry.
fn is_user(tf: &TrapFrame) -> bool {
    tf.sstatus.spp() == sstatus::SPP::User
}

/// Advances the saved program counter past a breakpoint instruction.
///
/// The low instruction bits distinguish compressed and ordinary encodings.
fn handle_breakpoint(sepc: &mut usize) {
    // SAFETY: `sepc` identifies the mapped instruction that raised the breakpoint exception.
    let instruction = unsafe { va!(*sepc).as_ptr_of::<u16>().read_volatile() };
    *sepc += if instruction & 0b11 == 0b11 { 4 } else { 2 };
}

/// Dispatches a RISC-V page fault to the common kernel callback.
///
/// An unhandled fault is fatal and includes the complete saved context in its diagnostic.
fn handle_page_fault(tf: &mut TrapFrame, mut flags: PageFaultFlags) -> TrapDisposition {
    let user = is_user(tf);
    if user {
        flags |= PageFaultFlags::USER;
    }

    let fault_addr = va!(stval::read());
    crate::trap::handle(
        tf,
        Trap::Exception(Exception::PageFault {
            address: fault_addr,
            flags,
            is_user: user,
        }),
    )
}

/// Dispatches one RISC-V supervisor trap.
///
/// This symbol is called directly by `trap.S` with a pointer to its stack-resident frame.
#[unsafe(no_mangle)]
extern "C" fn riscv_trap_handler(tf: &mut TrapFrame) {
    let scause = scause::read();

    let result = match scause.cause().try_into() {
        Ok(scause_decoded) => valid_riscv_trap_handler(tf, scause_decoded, scause.bits()),
        Err(e) => {
            error!(
                "Unknown RISC-V scause: {scause:#x} with stval={stval:#x} @ {sepc:#x}, {e:?}, continue anyway...",
                scause = scause.bits(),
                stval = stval::read(),
                sepc = tf.sepc,
            );

            unknown_riscv_trap_handler(tf, scause)
        }
    };

    if result != TrapDisposition::Handled {
        warn!(
            "Unhandled RISC-V trap: {scause:#x} with stval={stval:#x} @ {sepc:#x}",
            scause = scause.bits(),
            stval = stval::read(),
            sepc = tf.sepc
        );
    }
}

/// Handles a RISC-V trap that is known to the kernel.
fn valid_riscv_trap_handler(
    tf: &mut TrapFrame,
    scause: RiscVTrap<RiscVInterrupt, RiscVException>,
    raw_bits: usize,
) -> TrapDisposition {
    match scause {
        RiscVTrap::Interrupt(RiscVInterrupt::SupervisorTimer) => {
            crate::trap::handle(tf, Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)))
        }
        RiscVTrap::Interrupt(RiscVInterrupt::SupervisorExternal) => {
            super::trap::handle_external(tf)
        }
        RiscVTrap::Interrupt(RiscVInterrupt::SupervisorSoft) => {
            let disposition = crate::trap::handle(
                tf,
                Trap::Interrupt(Interrupt::Local(LocalInterrupt::Software)),
            );

            if disposition == TrapDisposition::Unhandled {
                supervisor::disable_interrupt(RiscVInterrupt::SupervisorSoft);
            }

            disposition
        }
        RiscVTrap::Exception(RiscVException::InstructionPageFault) => {
            handle_page_fault(tf, PageFaultFlags::EXECUTE)
        }
        RiscVTrap::Exception(RiscVException::LoadPageFault) => {
            handle_page_fault(tf, PageFaultFlags::READ)
        }
        RiscVTrap::Exception(RiscVException::StorePageFault) => {
            handle_page_fault(tf, PageFaultFlags::WRITE)
        }
        RiscVTrap::Exception(RiscVException::Breakpoint) => {
            handle_breakpoint(&mut tf.sepc);
            crate::trap::handle(tf, Trap::Exception(Exception::Breakpoint))
        }
        RiscVTrap::Exception(RiscVException::IllegalInstruction) => {
            crate::trap::handle(tf, Trap::Exception(Exception::InvalidInstruction))
        }
        RiscVTrap::Exception(other) => {
            warn!(
                "Unsupported RISC-V exception: {other:?} with stval={:#x} @ {:#x}",
                stval::read(),
                tf.sepc
            );
            crate::trap::handle(
                tf,
                Trap::Exception(Exception::Unknown(crate::trap::RawTrap(raw_bits))),
            )
        }
    }
}

/// Handles a RISC-V trap that is unknown to the kernel.
fn unknown_riscv_trap_handler(tf: &mut TrapFrame, scause: Scause) -> TrapDisposition {
    let raw_trap = RawTrap(scause.bits());

    crate::trap::handle(
        tf,
        if scause.is_exception() {
            Trap::Exception(Exception::Unknown(raw_trap))
        } else {
            Trap::Interrupt(Interrupt::Unknown(raw_trap))
        },
    )
}
