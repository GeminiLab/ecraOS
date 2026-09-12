//! AArch64 task and trap context frames.
//!
//! The task-context layout is an assembly ABI shared with `switch.S`. CPU-local state, including
//! `TPIDR_EL1`, deliberately remains outside a task context.

use core::{arch::global_asm, mem::offset_of};

use memory_addr::{MemoryAddr, VirtAddr, va};

use crate::trap::TrapFrameAccess;

global_asm!(include_str!("switch.S"));

/// Saved callee-preserved registers for an AArch64 kernel task.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskContext {
    /// Saved stack pointer.
    pub sp: usize,
    /// Saved callee-preserved register `x19`.
    pub x19: usize,
    /// Saved callee-preserved register `x20`.
    pub x20: usize,
    /// Saved callee-preserved register `x21`.
    pub x21: usize,
    /// Saved callee-preserved register `x22`.
    pub x22: usize,
    /// Saved callee-preserved register `x23`.
    pub x23: usize,
    /// Saved callee-preserved register `x24`.
    pub x24: usize,
    /// Saved callee-preserved register `x25`.
    pub x25: usize,
    /// Saved callee-preserved register `x26`.
    pub x26: usize,
    /// Saved callee-preserved register `x27`.
    pub x27: usize,
    /// Saved callee-preserved register `x28`.
    pub x28: usize,
    /// Saved frame-pointer register `x29`.
    pub x29: usize,
    /// Saved link register `x30`.
    pub x30: usize,
}

/// Initializes an AArch64 task context with a direct entry trampoline.
///
/// # Safety
///
/// `stack_top` must identify a 16-byte-aligned writable stack. `entry` and `arg` must remain valid
/// until the task exits.
pub unsafe fn init_task_context(
    context: &mut TaskContext,
    stack_top: VirtAddr,
    entry: unsafe extern "C" fn(usize) -> !,
    arg: usize,
) {
    *context = TaskContext::default();
    context.sp = stack_top.align_down(16usize).as_usize();
    context.x19 = arg;
    context.x20 = entry as *const () as usize;
    context.x30 = context_entry_trampoline as *const () as usize;
}

/// Switches AArch64 callee-preserved task state.
///
/// # Safety
///
/// Local interrupts must be disabled and both context pointers must remain valid for the switch.
pub unsafe fn switch(previous: *mut TaskContext, next: *const TaskContext) {
    unsafe extern "C" {
        /// Performs the raw AArch64 context switch.
        fn exarch_task_switch(previous: *mut TaskContext, next: *const TaskContext);
    }
    // SAFETY: The caller guarantees the assembly ABI's pointer requirements.
    unsafe { exarch_task_switch(previous, next) };
}

unsafe extern "C" {
    /// Enters an initialized AArch64 task context.
    fn context_entry_trampoline() -> !;
}

const _: () = {
    assert!(core::mem::align_of::<TaskContext>() == core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<TaskContext>() == 13 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, sp) == 0);
    assert!(offset_of!(TaskContext, x19) == 1 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, x30) == 12 * core::mem::size_of::<usize>());
};

/// Saved AArch64 general registers and EL1 exception state.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TrapFrame {
    /// General-purpose registers `x0` through `x30`.
    pub x: [usize; 31],
    /// Saved EL0 stack pointer.
    pub sp_el0: usize,
    /// Saved exception return address.
    pub elr_el1: usize,
    /// Saved processor state.
    pub spsr_el1: usize,
    /// Saved exception syndrome.
    pub esr_el1: usize,
    /// Saved fault address.
    pub far_el1: usize,
}

impl TrapFrameAccess for TrapFrame {
    fn instruction_pointer(&self) -> VirtAddr {
        va!(self.elr_el1)
    }

    fn set_instruction_pointer(&mut self, instruction_pointer: VirtAddr) {
        self.elr_el1 = instruction_pointer.as_usize();
    }

    fn is_user(&self) -> bool {
        self.spsr_el1 & 0b1111 == 0
    }
}
