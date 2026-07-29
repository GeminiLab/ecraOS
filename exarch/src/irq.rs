//! Interrupt registration, dispatch, and local CPU control.
//!
//! Architecture implementations may support only a subset of interrupt numbers.

use crate_interface::def_interface;

/// A statically registered interrupt handler.
///
/// Handlers run with local interrupts disabled and must not block or allocate.
pub type IrqHandler = fn();

/// Architecture interrupt operations.
///
/// Each architecture reports unsupported registration and dispatch explicitly.
#[def_interface(gen_caller)]
pub trait IrqIf {
    /// Registers `handler` for `irq`.
    ///
    /// Returns `false` when the IRQ is unsupported or already registered.
    fn register(irq: usize, handler: IrqHandler) -> bool;

    /// Unregisters the handler for `irq`.
    ///
    /// Returns the previous handler, or `None` when none was registered.
    fn unregister(irq: usize) -> Option<IrqHandler>;

    /// Dispatches `irq` to its registered handler.
    ///
    /// Returns whether a handler accepted the IRQ.
    fn handle(irq: usize) -> bool;

    /// Enables interrupts globally on the current CPU.
    ///
    /// Callers must ensure all interrupt sources have valid handlers.
    fn enable_local();

    /// Disables interrupts globally on the current CPU.
    ///
    /// Interrupt source enable bits are left unchanged.
    fn disable_local();

    /// Returns whether interrupts are globally enabled on the current CPU.
    ///
    /// This does not inspect individual interrupt source masks.
    fn local_enabled() -> bool;
}

#[cfg(target_arch = "riscv64")]
pub use crate::arch::current::irq::{EXTERNAL_IRQ_NUM, SOFTWARE_IRQ_NUM, TIMER_IRQ_NUM};
