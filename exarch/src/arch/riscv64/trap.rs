//! RISC-V supervisor trap handling.
//!
//! This module installs the direct trap vector and dispatches traps to common kernel callbacks.

use core::arch::global_asm;

use memory_addr::va;
use riscv::{
    interrupt::Trap,
    register::{scause, sstatus, stval, stvec},
};

use super::context::TrapFrame;
use crate::trap::PageFaultFlags;

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
    super::irq::init_percpu();
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
fn handle_page_fault(tf: &TrapFrame, mut flags: PageFaultFlags) {
    let user = is_user(tf);
    if user {
        flags |= PageFaultFlags::USER;
    }

    let fault_addr = va!(stval::read());
    if !crate::trap::handle_page_fault(fault_addr, flags, user) {
        panic!(
            "Unhandled {} page fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}",
            if user { "user" } else { "supervisor" },
            tf.sepc,
            fault_addr,
            flags,
            tf,
        );
    }
}

/// Dispatches one RISC-V supervisor trap.
///
/// This symbol is called directly by `trap.S` with a pointer to its stack-resident frame.
#[unsafe(no_mangle)]
extern "C" fn riscv_trap_handler(tf: &mut TrapFrame) {
    let scause = scause::read();
    match scause.cause() {
        Trap::Interrupt(_) => {
            if !crate::trap::handle_irq(scause.bits()) {
                panic!(
                    "Unhandled IRQ {:#x} @ {:#x}:\n{:#x?}",
                    scause.bits(),
                    tf.sepc,
                    tf
                );
            }
        }
        Trap::Exception(12) => handle_page_fault(tf, PageFaultFlags::EXECUTE),
        Trap::Exception(13) => handle_page_fault(tf, PageFaultFlags::READ),
        Trap::Exception(15) => handle_page_fault(tf, PageFaultFlags::WRITE),
        Trap::Exception(3) => handle_breakpoint(&mut tf.sepc),
        other => panic!(
            "Unhandled trap {:?}, stval={:#x} @ {:#x}:\n{:#x?}",
            other,
            stval::read(),
            tf.sepc,
            tf,
        ),
    }
}
