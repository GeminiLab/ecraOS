//! x86-64 local interrupt control.
//!
//! IRQ registration remains unsupported until the x86 controller path adopts the common API.

use x86_64::instructions::interrupts;

use crate::irq::IrqIf;

/// The x86-64 implementation of local interrupt operations.
///
/// Handler registration reports unsupported until the controller path adopts the common API.
pub struct IrqImpl;

#[crate_interface::impl_interface]
impl IrqIf for IrqImpl {
    fn register(_irq: usize, _handler: crate::irq::IrqHandler) -> bool {
        false
    }

    fn unregister(_irq: usize) -> Option<crate::irq::IrqHandler> {
        None
    }

    fn handle(_irq: usize) -> bool {
        false
    }

    fn enable_local() {
        interrupts::enable();
    }

    fn disable_local() {
        interrupts::disable();
    }

    fn local_enabled() -> bool {
        interrupts::are_enabled()
    }
}
