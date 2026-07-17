use log::{error, info};
use memory_addr::{PhysAddr, VirtAddr};

use crate::power::{APEntry, PhysicalCpuId, PowerIf};

struct PowerImpl;

#[crate_interface::impl_interface]
impl PowerIf for PowerImpl {
    fn current_cpu_id() -> PhysicalCpuId {
        todo!()
    }

    fn cpu_up(
        phys_id: PhysicalCpuId,
        page_table_root: PhysAddr,
        boot_stack_top: VirtAddr,
        entry: APEntry,
    ) {
        todo!()
    }

    fn poweroff() -> ! {
        info!("Shutting down...");
        sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason);
        error!("It should shutdown!");
        loop {
            core::hint::spin_loop();
        }
    }
}
