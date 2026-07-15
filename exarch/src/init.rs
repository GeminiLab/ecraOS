//! Initialization hooks: early and later platform bring-up.

pub use exboot::PlatformBootArg;

use crate_interface::def_interface;

/// Platform initialization contract invoked from portable kernel code.
#[def_interface(gen_caller)]
pub trait InitIf {
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
    fn init_early(arg: PlatformBootArg);
    /// Later platform initialization. Yet to be implemented.
    fn init_later(arg: PlatformBootArg);

    fn init_early_ap();
}
