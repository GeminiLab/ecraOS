//! Context frames.
//!
//! Original code from `axcpu` v0.3.1.

use core::arch::global_asm;
use core::mem::offset_of;

use memory_addr::{MemoryAddr, VirtAddr, va};

use crate::trap::TrapFrameAccess;

global_asm!(include_str!("switch.S"), options(att_syntax));

/// Saved registers required to resume an x86-64 kernel task.
///
/// The layout is shared with `switch.S` and excludes the `expercpu`-owned GS base.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskContext {
    /// Saved stack pointer.
    ///
    /// The switch assembly resumes execution from this task-owned stack.
    pub rsp: usize,
    /// Saved callee-saved register `rbx`.
    ///
    /// The switch assembly preserves `rbx` according to the Rust ABI.
    pub rbx: usize,
    /// Saved frame pointer.
    ///
    /// The switch assembly preserves `rbp` according to the Rust ABI.
    pub rbp: usize,
    /// Saved callee-saved register `r12`.
    ///
    /// The switch assembly preserves `r12` according to the Rust ABI.
    pub r12: usize,
    /// Saved callee-saved register `r13`.
    ///
    /// The switch assembly preserves `r13` according to the Rust ABI.
    pub r13: usize,
    /// Saved callee-saved register `r14`.
    ///
    /// The switch assembly preserves `r14` according to the Rust ABI.
    pub r14: usize,
    /// Saved callee-saved register `r15`.
    ///
    /// The switch assembly preserves `r15` according to the Rust ABI.
    pub r15: usize,
}

/// Initializes an x86-64 task context with a stack bootstrap frame.
///
/// # Safety
///
/// `stack_top` must identify a 16-byte-aligned stack with at least 24 writable bytes below it.
/// `entry` and the resources represented by `arg` must remain valid until the task exits.
pub unsafe fn init_task_context(
    context: &mut TaskContext,
    stack_top: VirtAddr,
    entry: unsafe extern "C" fn(usize) -> !,
    arg: usize,
) {
    *context = TaskContext::default();
    let stack_top = stack_top.align_down(16usize);
    let stack_args: *mut usize = stack_top.sub(24).as_mut_ptr_of();
    // SAFETY: The caller guarantees writable space for this frame.
    unsafe {
        stack_args
            .add(0)
            .write(context_entry_trampoline as *const () as usize);
        stack_args.add(1).write(arg);
        stack_args.add(2).write(entry as *const () as usize);
    }
    context.rsp = stack_args as usize;
}

/// Switches x86-64 callee-saved task state.
///
/// # Safety
///
/// Local interrupts must be disabled. Both pointers must be aligned, valid, and uniquely writable
/// where appropriate until this task is resumed.
pub unsafe fn switch(previous: *mut TaskContext, next: *const TaskContext) {
    unsafe extern "C" {
        /// Performs the raw x86-64 context switch.
        ///
        /// The implementation saves `previous` and restores `next` using the shared assembly ABI.
        fn exarch_task_switch(previous: *mut TaskContext, next: *const TaskContext);
    }
    // SAFETY: The caller guarantees valid context pointers.
    unsafe { exarch_task_switch(previous, next) };
}

unsafe extern "C" {
    /// Enters an initialized x86-64 task context.
    ///
    /// The assembly trampoline extracts the entry argument and non-returning function from stack.
    fn context_entry_trampoline() -> !;
}

const _: () = {
    assert!(core::mem::align_of::<TaskContext>() == core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<TaskContext>() == 7 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, rsp) == 0);
    assert!(offset_of!(TaskContext, rbx) == core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, rbp) == 2 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, r12) == 3 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, r13) == 4 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, r14) == 5 * core::mem::size_of::<usize>());
    assert!(offset_of!(TaskContext, r15) == 6 * core::mem::size_of::<usize>());
};

/// Saved registers when a trap (interrupt or exception) occurs.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TrapFrame {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    // Pushed by `trap.S`
    pub vector: u64,
    pub error_code: u64,

    // Pushed by CPU
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.rdi as _
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.rsi as _
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.rdx as _
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.r10 as _
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.r8 as _
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.r9 as _
    }

    /// Whether the trap is from userspace.
    pub const fn is_user(&self) -> bool {
        self.cs & 0b11 == 3
    }
}

impl TrapFrameAccess for TrapFrame {
    fn instruction_pointer(&self) -> VirtAddr {
        va!(self.rip as usize)
    }

    fn set_instruction_pointer(&mut self, instruction_pointer: VirtAddr) {
        self.rip = instruction_pointer.as_usize() as u64;
    }

    fn is_user(&self) -> bool {
        TrapFrame::is_user(self)
    }
}
