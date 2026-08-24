//! RISC-V trap context frames.
//!
//! Field order is part of the assembly ABI used by `trap.S`.

use core::arch::global_asm;
use core::mem::offset_of;

use memory_addr::{MemoryAddr, VirtAddr, va};
use riscv::register::sstatus;

use crate::trap::TrapFrameAccess;

global_asm!(include_str!("switch.S"));

/// Saved registers required to resume a RISC-V 64 kernel task.
///
/// The layout is shared with `switch.S` and excludes the `expercpu`-owned `gp` register.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskContext {
    /// Saved return address.
    ///
    /// The switch assembly resumes execution at this address.
    pub ra: usize,
    /// Saved stack pointer.
    ///
    /// The switch assembly resumes execution from this task-owned stack.
    pub sp: usize,
    /// Saved callee-saved register `s0`.
    ///
    /// The switch assembly preserves `s0` according to the Rust ABI.
    pub s0: usize,
    /// Saved callee-saved register `s1`.
    ///
    /// The switch assembly preserves `s1` according to the Rust ABI.
    pub s1: usize,
    /// Saved callee-saved register `s2`.
    ///
    /// The switch assembly preserves `s2` according to the Rust ABI.
    pub s2: usize,
    /// Saved callee-saved register `s3`.
    ///
    /// The switch assembly preserves `s3` according to the Rust ABI.
    pub s3: usize,
    /// Saved callee-saved register `s4`.
    ///
    /// The switch assembly preserves `s4` according to the Rust ABI.
    pub s4: usize,
    /// Saved callee-saved register `s5`.
    ///
    /// The switch assembly preserves `s5` according to the Rust ABI.
    pub s5: usize,
    /// Saved callee-saved register `s6`.
    ///
    /// The switch assembly preserves `s6` according to the Rust ABI.
    pub s6: usize,
    /// Saved callee-saved register `s7`.
    ///
    /// The switch assembly preserves `s7` according to the Rust ABI.
    pub s7: usize,
    /// Saved callee-saved register `s8`.
    ///
    /// The switch assembly preserves `s8` according to the Rust ABI.
    pub s8: usize,
    /// Saved callee-saved register `s9`.
    ///
    /// The switch assembly preserves `s9` according to the Rust ABI.
    pub s9: usize,
    /// Saved callee-saved register `s10`.
    ///
    /// The switch assembly preserves `s10` according to the Rust ABI.
    pub s10: usize,
    /// Saved callee-saved register `s11`.
    ///
    /// The switch assembly preserves `s11` according to the Rust ABI.
    pub s11: usize,
}

/// Initializes a RISC-V 64 task context with a stack bootstrap frame.
///
/// # Safety
///
/// `stack_top` must identify a 16-byte-aligned stack with at least 16 writable bytes below it.
/// `entry` and the resources represented by `arg` must remain valid until the task exits.
pub unsafe fn init_task_context(
    context: &mut TaskContext,
    stack_top: VirtAddr,
    entry: unsafe extern "C" fn(usize) -> !,
    arg: usize,
) {
    *context = TaskContext::default();
    let stack_top = stack_top.align_down(16usize);
    let stack_args: *mut usize = stack_top.sub(16).as_mut_ptr_of();
    // SAFETY: The caller guarantees writable space for this frame.
    unsafe {
        stack_args.add(0).write(arg);
        stack_args.add(1).write(entry as *const () as usize);
    }
    context.ra = context_entry_trampoline as *const () as usize;
    context.sp = stack_args as usize;
}

/// Switches RISC-V 64 callee-saved task state.
///
/// # Safety
///
/// Local interrupts must be disabled. Both pointers must be aligned, valid, and uniquely writable
/// where appropriate until this task is resumed.
pub unsafe fn switch(previous: *mut TaskContext, next: *const TaskContext) {
    unsafe extern "C" {
        /// Performs the raw RISC-V 64 context switch.
        ///
        /// The implementation saves `previous` and restores `next` using the shared assembly ABI.
        fn exarch_task_switch(previous: *mut TaskContext, next: *const TaskContext);
    }
    // SAFETY: The caller guarantees valid context pointers.
    unsafe { exarch_task_switch(previous, next) };
}

unsafe extern "C" {
    /// Enters an initialized RISC-V 64 task context.
    ///
    /// The assembly trampoline extracts the entry argument and non-returning function from stack.
    fn context_entry_trampoline() -> !;
}

const _: () = {
    assert!(core::mem::align_of::<TaskContext>() == core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<TaskContext>() == 14 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, ra) == 0);
    assert!(offset_of!(TaskContext, sp) == core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s0) == 2 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s1) == 3 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s2) == 4 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s3) == 5 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s4) == 6 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s5) == 7 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s6) == 8 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s7) == 9 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s8) == 10 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s9) == 11 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s10) == 12 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, s11) == 13 * core::mem::size_of::<usize>());
};

/// The RISC-V integer register file saved during a trap.
///
/// The zero slot keeps field offsets equal to architectural register numbers.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub zero: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize,
    pub tp: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub s0: usize,
    pub s1: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
}

/// The saved RISC-V supervisor trap frame.
///
/// Assembly restores `sepc` and `sstatus` from this frame before `sret`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// The saved integer registers.
    pub regs: GeneralRegisters,
    /// The saved supervisor exception program counter.
    pub sepc: usize,
    /// The saved supervisor status register.
    pub sstatus: sstatus::Sstatus,
}

impl Default for TrapFrame {
    fn default() -> Self {
        Self {
            regs: GeneralRegisters::default(),
            sepc: 0,
            sstatus: sstatus::Sstatus::from_bits(0),
        }
    }
}

impl TrapFrameAccess for TrapFrame {
    fn instruction_pointer(&self) -> VirtAddr {
        va!(self.sepc)
    }

    fn set_instruction_pointer(&mut self, instruction_pointer: VirtAddr) {
        self.sepc = instruction_pointer.as_usize();
    }

    fn is_user(&self) -> bool {
        self.sstatus.spp() == sstatus::SPP::User
    }
}

const _: () =
    assert!(core::mem::size_of::<GeneralRegisters>() == 32 * core::mem::size_of::<usize>());
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 34 * core::mem::size_of::<usize>());
