use exarch::power::PhysicalCpuId;
use memory_addr::{PhysAddr, VirtAddr};

struct KernelImpl;

#[crate_interface::impl_interface]
impl kernel_guard::KernelGuardIf for KernelImpl {
    fn enable_preempt() {
        crate::task::preempt::enable_preempt();
    }

    fn disable_preempt() {
        crate::task::preempt::disable_preempt();
    }
}

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
