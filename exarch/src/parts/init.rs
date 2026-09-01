//! Platform initialization functions.

pub use ecraldr_base::PlatformBootArg;

use look_at::look_at;

look_at! {
    @crate::arch::current::init:

    /// Early platform initialization.
    ///
    /// This method should be called immediately after the kernel entry runs,
    /// and should perform the early platform initialization. This method will
    /// be called only once, on the bootstrap processor.
    ///
    /// When this method is called, the following conditions are met:
    ///
    /// - The CPU is in its desired working state (for example, in long mode in
    ///   the x86-64 architecture).
    /// - Virtual memory and paging are enabled with an identity mapping.
    /// - Allocation is not yet available.
    /// - PerCPU data area for the BSP is available.
    /// - Interrupts are disabled.
    ///
    /// This method should:
    /// - Initialize the debug console.
    /// - Initialize the interrupt controller and interrupt handling, while keeping interrupts
    ///   disabled.
    /// - Initialize the time module.
    /// - Prepare for calling other initialization functions.
    pub fn init_early(arg: PlatformBootArg);
    /// Later platform initialization. Yet to be implemented.
    pub fn init_later();

    /// Early platform initialization for APs.
    pub fn init_early_ap();
    /// Later platform initialization for APs.
    pub fn init_later_ap();
}
