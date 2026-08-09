//! Kernel-owned global interrupt source handlers.
//!
//! Architecture code supplies typed source identities and hardware lifecycle operations. This
//! module owns device handler registration, enabled state, and unhandled accounting.

use exarch::{
    irq::{self, GlobalIrq, IrqError, IrqHandler, Registry},
    trap::{TrapDisposition, TrapFrame},
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
pub fn register(irq: GlobalIrq, handler: IrqHandler) -> Result<(), IrqError> {
    irq::prepare_global(irq)?;
    REGISTRY
        .get()
        .ok_or(IrqError::Unsupported)?
        .register(irq, handler)
}

/// Unregisters a kernel device handler after disabling its hardware source.
///
/// The source remains masked until a later explicit registration and enable operation.
pub fn unregister(irq: GlobalIrq) -> Result<IrqHandler, IrqError> {
    irq::set_global_enabled(irq, false)?;
    REGISTRY.get().ok_or(IrqError::Unsupported)?.unregister(irq)
}

/// Enables or disables a registered kernel device source.
///
/// Disabling masks hardware before clearing logical delivery, while enabling publishes logical
/// state before unmasking the controller source.
pub fn set_enabled(irq: GlobalIrq, enabled: bool) -> Result<(), IrqError> {
    if !enabled {
        irq::set_global_enabled(irq, false)?;
    }
    REGISTRY
        .get()
        .ok_or(IrqError::Unsupported)?
        .set_enabled(irq, enabled)?;
    if enabled {
        irq::set_global_enabled(irq, true)?;
    }
    Ok(())
}

/// Dispatches one claimed global source to its kernel device handler.
///
/// The architecture adapter masks an unhandled result before it acknowledges or completes the
/// hardware delivery.
pub fn dispatch(frame: &mut TrapFrame, irq: GlobalIrq) -> TrapDisposition {
    let _ = frame;
    match REGISTRY.get() {
        Some(registry) => registry
            .dispatch(irq)
            .map(|_| TrapDisposition::Unhandled)
            .unwrap_or(TrapDisposition::Handled),
        None => TrapDisposition::Unhandled,
    }
}

/// Returns the unregistered-delivery count for one global source.
///
/// This diagnostic is owned by the kernel registry and does not represent a CPU-local vector.
pub fn unhandled_count(irq: GlobalIrq) -> usize {
    REGISTRY
        .get()
        .map(|registry| registry.unhandled_count(irq))
        .unwrap_or(0)
}

/// Unmasks a source for a controller self-test without changing kernel registration state.
///
/// The self-test uses this only after unregistering a source to verify unhandled accounting.
pub fn test_unmask(irq: GlobalIrq) -> Result<(), IrqError> {
    irq::test_unmask(irq)
}

/// Masks a source after a controller self-test.
///
/// This leaves the kernel source disabled and hardware quiescent.
pub fn test_mask(irq: GlobalIrq) -> Result<(), IrqError> {
    irq::test_mask(irq)
}
