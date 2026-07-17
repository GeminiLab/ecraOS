//! Trap handling.

use crate_interface::def_interface;
use memory_addr::VirtAddr;

#[cfg(target_arch = "x86_64")]
pub use crate::TrapFrame;
pub use page_table_entry::MappingFlags as PageFaultFlags;

#[def_interface(gen_caller)]
pub trait TrapHandler {
    fn handle_irq(irq: usize) -> bool {
        _ = irq;
        false
    }
    fn handle_page_fault(addr: VirtAddr, flags: PageFaultFlags, write: bool) -> bool {
        (_, _, _) = (addr, flags, write);
        false
    }
}
