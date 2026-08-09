//! Power management.

use look_at::look_at;
use memory_addr::{PhysAddr, VirtAddr};

/// The physical identifier assigned to a CPU by firmware.
///
/// Architecture startup mechanisms use this identifier when addressing a hardware CPU.
pub type PhysicalCpuId = usize;

/// The common entry point used to initialize an application CPU.
///
/// The entry receives the physical CPU identifier and does not return after common initialization.
pub type APEntry = unsafe extern "Rust" fn(hart_id: usize) -> !;

/// A failure reported while requesting architecture CPU startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuStartError {
    /// The platform does not expose the required startup mechanism.
    Unsupported,
    /// Firmware rejected the startup request.
    FirmwareDenied,
    /// The requested physical CPU identifier is invalid.
    InvalidCpu,
    /// The startup request timed out.
    Timeout,
    /// Something went wrong with the boot stack allocation or mapping.
    BootStackError,
    /// The CPU lifecycle state does not permit the requested startup transition.
    ///
    /// This error indicates that the kernel attempted to start a CPU outside the state expected by
    /// its startup protocol.
    InvalidState,
    /// The platform transport returned an otherwise unspecified error.
    Transport,
}

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
///
/// The wrapper forwards calls to the selected architecture implementation.
#[look_at(crate::arch::current::power, flatten)]
mod _wrapper {
    /// Brings up a physical CPU.
    ///
    /// The return value reports whether the architecture accepted the startup request.
    pub fn cpu_up(
        phys_id: PhysicalCpuId,
        page_table_root: PhysAddr,
        boot_stack_top: VirtAddr,
        entry: APEntry,
    ) -> Result<(), CpuStartError>;

    /// Terminates the current kernel execution.
    ///
    /// Current platform implementations perform their ordinary shutdown and reserve the reason for
    /// future policy.
    pub fn shutdown(reason: ShutdownReason) -> !;
}

/// Powers the system off through the common shutdown interface.
///
/// Existing normal callers use this wrapper instead of selecting a terminal reason themselves.
pub fn poweroff() -> ! {
    shutdown(ShutdownReason::PowerOff)
}
