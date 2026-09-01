//! Interrupt registration, dispatch, and local CPU control.
//!
//! Architecture implementations may support only a subset of interrupt numbers.

use alloc::boxed::Box;

use lazyinit::LazyInit;
use look_at::look_at;

mod registry;

pub use crate::trap::GlobalIrq;
pub use registry::Registry;
pub use registry::UnhandledReason;

#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::imp::ioapic::{IoApicConfig, Polarity, TriggerMode};

#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::irq::{X86ExternalIrqConfig, X86IrqOverride};

#[cfg(target_arch = "riscv64")]
pub use crate::arch::riscv64::irq::{RiscvExternalIrqConfig, RiscvPlicContext};

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

/// Prepares a typed source route while keeping the architecture placeholder masked.
///
/// Kernel device handlers live in `ecraos::global_irq`, while the architecture only programs the
/// controller route.
pub fn prepare_global(irq: GlobalIrq) -> Result<(), IrqError> {
    prepare(irq)
}

/// Changes the hardware mask state for a prepared source.
pub fn set_global_enabled(irq: GlobalIrq, enabled: bool) -> Result<(), IrqError> {
    set_enabled(irq, enabled)
}

/// Initializes the current architecture's external interrupt controller.
#[cfg(target_arch = "x86_64")]
pub fn init_external_controller(config: X86ExternalIrqConfig) {
    crate::arch::x86_64::irq::init_external(config);
}

/// Initializes the current architecture's external interrupt controller.
#[cfg(target_arch = "riscv64")]
pub fn init_external_controller(config: RiscvExternalIrqConfig) {
    crate::arch::riscv64::irq::init_external(config);
}

look_at! {
    @crate::arch::current::irq:
    /// Enables interrupts globally on the current CPU.
    ///
    /// Callers must ensure all interrupt sources have valid handlers.
    pub fn enable_local();

    /// Disables interrupts globally on the current CPU.
    ///
    /// Interrupt source enable bits are left unchanged.
    pub fn disable_local();

    /// Returns whether interrupts are globally enabled on the current CPU.
    ///
    /// This does not inspect individual interrupt source masks.
    pub fn local_enabled() -> bool;

    /// Prepares a common external source while keeping it masked.
    pub fn prepare(irq: GlobalIrq) -> Result<(), IrqError>;

    /// Enables or disables a registered common external source.
    pub fn set_enabled(irq: GlobalIrq, enabled: bool) -> Result<(), IrqError>;
}

/// A statically registered device interrupt handler.
///
/// The kernel invokes this function from its typed GlobalIrq source table with local interrupts
/// disabled, and handlers must not block, allocate, or synchronously unregister themselves.
pub type IrqHandler = fn();

#[cfg(target_arch = "riscv64")]
pub use crate::arch::current::irq::{EXTERNAL_IRQ_NUM, SOFTWARE_IRQ_NUM, TIMER_IRQ_NUM};

#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::irq::TIMER_IRQ_NUM;
