//! Kernel semantic trap handlers.
//!
//! This module owns the three semantic handler registrations and adapts the global handler to the
//! existing controller source registry while that registry is being moved into the kernel.

pub mod global;

use exarch::trap::{
    Exception, ExceptionHandler, GlobalIrqHandler, HandlerSlots, Trap, TrapDisposition, TrapFrame,
    TrapFrameAccess, TrapHandler,
};

/// The kernel-owned semantic handler slots.
///
/// The slots are initialized during boot and may be replaced or cleared during controlled
/// lifecycle transitions.
static HANDLERS: HandlerSlots = HandlerSlots::new();

/// Registers or replaces one semantic kernel handler.
///
/// Registration must complete before the corresponding local interrupt source is enabled. Handler
/// slots may be replaced or cleared during controlled lifecycle transitions.
pub fn register<H: TrapHandler>(handler: H) {
    HANDLERS.register(handler)
}

/// Dispatches a decoded semantic trap through the kernel-owned handler slots.
///
/// Unknown interrupt values are completed by architecture code and never enter this function.
pub fn handle(frame: &mut TrapFrame, trap: Trap) -> TrapDisposition {
    HANDLERS.dispatch(frame, trap)
}

/// Bridges architecture trap entry to the kernel semantic handler table.
///
/// The implementation is generated as the `exarch::trap` crate-interface provider.
struct TrapHandlerImpl;

#[crate_interface::impl_interface]
impl exarch::trap::KernelTrapIf for TrapHandlerImpl {
    fn handle(frame: &mut TrapFrame, trap: Trap) -> TrapDisposition {
        crate::irq::handle(frame, trap)
    }
}

/// Handles exceptions that have no recoverable kernel subsystem yet.
///
/// Fatal exceptions panic directly, including unknown synchronous causes, so they cannot return
/// into a repeatedly faulting instruction.
fn handle_exception(frame: &mut TrapFrame, exception: Exception) -> TrapDisposition {
    match exception {
        Exception::Breakpoint => {
            log::debug!("#BP @ {:?}", frame.instruction_pointer());
            TrapDisposition::Handled
        }
        exception => panic!(
            "Unhandled exception {:?} @ {:?}, user={}",
            exception,
            frame.instruction_pointer(),
            frame.is_user()
        ),
    }
}

/// Handles global interrupts through the existing architecture source registry.
///
/// The registry migration keeps source handlers in the kernel-facing boundary while preserving the
/// tested controller lifecycle during this transition.
fn handle_global_irq(frame: &mut TrapFrame, irq: exarch::trap::GlobalIrq) -> TrapDisposition {
    global::dispatch(frame, irq)
}

/// Installs the exception and global semantic handlers.
///
/// The local-interrupt slot is installed by [`crate::timer::init_bsp`] before timer arming.
pub fn init() {
    global::init();

    _ = register::<ExceptionHandler>(handle_exception);
    _ = register::<GlobalIrqHandler>(handle_global_irq);
}
