//! Global IRQ (device interrupts) handling.

use alloc::boxed::Box;

use lazyinit::LazyInit;
use look_at::look_at;

mod registry;

pub use registry::Registry;
pub use registry::UnhandledReason;

#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::imp::ioapic::{IoApicConfig, Polarity, TriggerMode};

#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::trap::{
    X86ExternalIrqConfig, X86ExternalIrqConfig as ArchIrqConfig, X86IrqOverride,
};

#[cfg(target_arch = "riscv64")]
pub use crate::arch::riscv64::trap::{
    RiscvExternalIrqConfig, RiscvExternalIrqConfig as ArchIrqConfig, RiscvPlicContext,
};

#[cfg(target_arch = "aarch64")]
pub use crate::arch::aarch64::trap::{
    Aarch64ExternalIrqConfig, Aarch64ExternalIrqConfig as ArchIrqConfig,
};

/// Reports an external IRQ lifecycle or platform validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqError {
    /// Reports that no external controller is available on the current platform.
    Unsupported,
    /// Reports that a handler already occupies the source.
    AlreadyRegistered,
    /// Reports that the source has no registered handler.
    NotRegistered,
    /// Reports that the source is outside the discovered valid domain.
    InvalidNumber,
}

/// The validated global interrupt source domain.
///
/// This metadata describes controller-valid source numbers but stores no kernel handlers.
static EXTERNAL_DOMAIN: LazyInit<Box<[bool]>> = LazyInit::new();

/// Initializes the common external registry from immutable controller validity metadata.
pub(crate) fn init_external_registry(valid: &[bool]) {
    EXTERNAL_DOMAIN.init_once(valid.to_vec().into_boxed_slice());
}

/// Returns the validated global-source domain discovered by the architecture controller.
///
/// Kernel source registration copies this immutable validity metadata into its own handler table.
pub fn external_irq_valid_sources() -> Result<&'static [bool], IrqError> {
    EXTERNAL_DOMAIN
        .get()
        .map(|domain| &**domain)
        .ok_or(IrqError::Unsupported)
}

/// A statically registered device interrupt handler.
///
/// The kernel invokes this function from its typed ArchIrq source table with local interrupts
/// disabled, and handlers must not block, allocate, or synchronously unregister themselves.
pub type IrqHandler = fn();

look_at! {
    @crate::arch::current::trap:

    /// A target-specific global interrupt source identifier.
    ///
    /// x86 aliases this type to a GSI and RISC-V aliases it to a PLIC source identifier. The
    /// distinct target-specific newtypes prevent local vectors and controller source identifiers
    /// from mixing.
    pub type ArchIrq;

    /// Prepares a common external source while keeping it masked.
    ///
    /// It usually means the architecture should program the controller route.
    pub fn prepare_irq(irq: ArchIrq) -> Result<(), IrqError>;

    /// Enables or disables a common external source.
    pub fn set_irq_enabled(irq: ArchIrq, enabled: bool) -> Result<(), IrqError>;

    /// Initializes the current architecture's external interrupt controller.
    pub fn init_external_controller(config: ArchIrqConfig);
}
