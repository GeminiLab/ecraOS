//! Kernel-owned global interrupt source handlers.
//!
//! Architecture code supplies typed source identities and hardware lifecycle operations. This
//! module owns device handler registration, enabled state, and unhandled accounting.

use exarch::{
    trap::irq::{self, ArchIrq, IrqError, IrqHandler, Registry},
    trap::{self, TrapDisposition, TrapFrame},
};
use lazyinit::LazyInit;

/// The kernel-owned global source registry.
///
/// The registry is initialized once from the validated architecture source domain.
static REGISTRY: LazyInit<Registry> = LazyInit::new();

/// Initializes the kernel global source registry from architecture validity metadata.
///
/// The source domain is immutable after controller discovery and excludes firmware-invalid holes.
pub fn init() {
    let valid = irq::external_irq_valid_sources().expect("external IRQ controller is absent");
    REGISTRY.init_once(Registry::new(valid));
}

/// Registers a kernel device handler while keeping its hardware source masked.
///
/// Route preparation is delegated to the architecture, while the callable handler is stored only
/// in this kernel-owned table.
pub fn register(irq: ArchIrq, handler: IrqHandler) -> Result<(), IrqError> {
    trap::irq::prepare_irq(irq)?;
    REGISTRY
        .get()
        .ok_or(IrqError::Unsupported)?
        .register(irq, handler)
}

/// Unregisters a kernel device handler after disabling its hardware source.
///
/// The source remains masked until a later explicit registration and enable operation.
pub fn unregister(irq: ArchIrq) -> Result<IrqHandler, IrqError> {
    trap::irq::set_irq_enabled(irq, false)?;
    REGISTRY.get().ok_or(IrqError::Unsupported)?.unregister(irq)
}

/// Enables or disables a registered kernel device source.
///
/// Disabling masks hardware before clearing logical delivery, while enabling publishes logical
/// state before unmasking the controller source.
pub fn set_enabled(irq: ArchIrq, enabled: bool) -> Result<(), IrqError> {
    if !enabled {
        trap::irq::set_irq_enabled(irq, false)?;
    }
    REGISTRY
        .get()
        .ok_or(IrqError::Unsupported)?
        .set_enabled(irq, enabled)?;
    if enabled {
        trap::irq::set_irq_enabled(irq, true)?;
    }
    Ok(())
}

/// Dispatches one claimed global source to its kernel device handler.
///
/// The architecture adapter masks an unhandled result before it acknowledges or completes the
/// hardware delivery.
pub fn dispatch(frame: &mut TrapFrame, irq: ArchIrq) -> TrapDisposition {
    let _ = frame;
    match REGISTRY.get() {
        Some(registry) => registry
            .dispatch(irq)
            .map(|_| TrapDisposition::Unhandled)
            .unwrap_or(TrapDisposition::Handled),
        None => TrapDisposition::Unhandled,
    }
}
