//! Common task context switching.

use memory_addr::VirtAddr;

look_at::look_at! {
    @crate::arch::current::context:

    /// A target-specific task context.
    pub type TaskContext;

    /// Initializes a task context to enter a non-returning function.
    ///
    /// The caller must provide an aligned, writable kernel stack and keep `entry` callable with `arg`.
    ///
    /// # Safety
    ///
    /// `stack_top` must meet the current architecture's stack alignment and bootstrap-frame space
    /// requirements. The entire stack, entry function, and resources represented by `arg` must remain
    /// live until the task has exited and can no longer be resumed.
    pub unsafe fn init_task_context(
        context: &mut TaskContext,
        stack_top: VirtAddr,
        entry: unsafe extern "C" fn(usize) -> !,
        arg: usize,
    );

    /// Switches from one task context to another.
    ///
    /// The caller must disable local interrupts and keep both context pointers valid throughout.
    ///
    /// # Safety
    ///
    /// `previous` must be uniquely writable while its task is suspended. `next`, its stack, and its
    /// return path must remain live, and the next task must not be running or switching on another CPU.
    /// Both pointers must meet the current architecture's alignment and lifetime requirements. The
    /// caller must ensure that no interrupt observes a partially switched context.
    pub unsafe fn switch(previous: *mut TaskContext, next: *const TaskContext);
}
