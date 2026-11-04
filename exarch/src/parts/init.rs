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
    /// - Initialize the early debug console.
    /// - Install the CPU's exception vector so synchronous exceptions can be handled.
    /// - Initialize architecture timekeeping needed by subsequent early setup.
    /// - Prepare other CPU-local state required before memory initialization and later architecture
    ///   initialization hooks.
    ///
    /// It must leave the current CPU able to take and handle exceptions. It does not initialize an
    /// interrupt controller or enable interrupt delivery.
    ///
    /// Architectures choose their own order. The recommended order is to initialize the debug
    /// console, install the exception vector, then perform other early initialization. Interrupt
    /// delivery must remain disabled.
    pub fn init_early(arg: PlatformBootArg);

    /// Initializes the global and BSP local interrupt controllers.
    ///
    /// The caller has completed platform discovery, mapped all reported MMIO regions, and
    /// published the discovered controller configuration. Implementations retain shared controller
    /// state needed by AP initialization.
    ///
    /// # Panics
    ///
    /// Panics if platform discovery did not publish the controller configuration.
    pub fn init_later();

    /// Installs the AP's exception path without enabling interrupt delivery.
    pub fn init_early_ap();

    /// Initializes the AP-local interrupt controller using BSP-published architecture state.
    pub fn init_later_ap();
}
