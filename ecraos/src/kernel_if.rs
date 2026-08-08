use exarch::power::PhysicalCpuId;
use memory_addr::{PhysAddr, VirtAddr};

struct KernelImpl;

#[crate_interface::impl_interface]
impl exarch::kernel_if::KernelIf for KernelImpl {
    fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
        crate::mem::virt_to_phys(addr)
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        crate::mem::phys_to_virt(addr)
    }

    fn current_cpu_phys_id() -> PhysicalCpuId {
        crate::mp::current_cpu_phys_id()
    }
}
