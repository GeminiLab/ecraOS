use crate_interface::def_interface;
use memory_addr::{PhysAddr, VirtAddr};

use crate::power::PhysicalCpuId;

/// Functions that the kernel provides to the architecture.
#[def_interface(gen_caller)]
pub trait KernelIf {
    /// Converts a virtual address into its physical address.
    fn virt_to_phys(addr: VirtAddr) -> PhysAddr;

    /// Converts a physical address into its currently accessible virtual address.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;

    /// Returns the physical CPU identifier of the current CPU.
    fn current_cpu_phys_id() -> PhysicalCpuId;
}
