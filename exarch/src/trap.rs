//! Trap handling.

use crate_interface::def_interface;
use memory_addr::VirtAddr;

pub use crate::TrapFrame;
pub use page_table_entry::MappingFlags as PageFaultFlags;

#[def_interface(gen_caller)]
pub trait TrapHandler {
    /// Handles an architecture interrupt cause.
    ///
    /// Returns whether the kernel accepted and completed the interrupt.
    fn handle_irq(irq: usize) -> bool {
        _ = irq;
        false
    }

    /// Handles a page fault at `addr` with the requested access flags.
    ///
    /// Returns whether the kernel resolved the fault for the originating privilege mode.
    fn handle_page_fault(addr: VirtAddr, flags: PageFaultFlags, is_user: bool) -> bool {
        (_, _, _) = (addr, flags, is_user);
        false
    }
}
