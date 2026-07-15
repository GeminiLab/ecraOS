//! Power management.

use crate_interface::def_interface;
use memory_addr::{PhysAddr, VirtAddr};

/// Physical CPU ID.
pub type PhysicalCpuId = usize;

pub type APEntry = unsafe extern "Rust" fn(hart_id: usize) -> !;

/// Power operations implemented by the platform.
#[def_interface(gen_caller)]
pub trait PowerIf {
    /// Returns the current physical CPU ID.
    fn current_cpu_id() -> PhysicalCpuId;

    /// Brings up a physical CPU.
    fn cpu_up(
        phys_id: PhysicalCpuId,
        page_table_root: PhysAddr,
        boot_stack_top: VirtAddr,
        entry: APEntry,
    );

    /// Turns the system off or halts it in a power-off state.
    fn poweroff() -> !;
}
