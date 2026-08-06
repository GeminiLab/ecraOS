//! Power management.

use crate_interface::def_interface;
use memory_addr::{PhysAddr, VirtAddr};

/// The physical identifier assigned to a CPU by firmware.
///
/// Architecture startup mechanisms use this identifier when addressing a hardware CPU.
pub type PhysicalCpuId = usize;

/// The common entry point used to initialize an application CPU.
///
/// The entry receives the physical CPU identifier and does not return after common initialization.
pub type APEntry = unsafe extern "Rust" fn(hart_id: usize) -> !;

/// A terminal kernel shutdown reason.
///
/// The variants reserve a common vocabulary for future platform-specific shutdown behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Terminates normal kernel execution.
    PowerOff,
    /// Terminates execution after a kernel failure.
    Panicked,
}

/// Power operations implemented by the platform.
#[def_interface(gen_caller)]
pub trait PowerIf {
    /// Returns the current physical CPU ID.
    ///
    /// The value identifies the hardware CPU executing the caller.
    fn current_cpu_id() -> PhysicalCpuId;

    /// Brings up a physical CPU.
    ///
    /// The return value reports whether the architecture accepted the startup request.
    fn cpu_up(
        phys_id: PhysicalCpuId,
        page_table_root: PhysAddr,
        boot_stack_top: VirtAddr,
        entry: APEntry,
    );

    /// Terminates the current kernel execution.
    ///
    /// Current platform implementations perform their ordinary shutdown and reserve the reason for
    /// future policy.
    fn shutdown(reason: ShutdownReason) -> !;
}

/// Powers the system off through the common shutdown interface.
///
/// Existing normal callers use this wrapper instead of selecting a terminal reason themselves.
pub fn poweroff() -> ! {
    shutdown(ShutdownReason::PowerOff)
}
