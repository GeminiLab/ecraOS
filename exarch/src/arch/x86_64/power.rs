//! Power-off via the QEMU ACPI PM control port (`0x604`).

use core::time::Duration;

use memory_addr::{PhysAddr, VirtAddr};

use crate::power::{APEntry, PhysicalCpuId, PowerIf, ShutdownReason};

/// The implementation of the [`PowerIf`] trait.
pub struct PowerImpl;

#[crate_interface::impl_interface]
impl PowerIf for PowerImpl {
    fn current_cpu_id() -> PhysicalCpuId {
        todo!()
    }

    fn cpu_up(
        cpu_id: PhysicalCpuId,
        page_table_root: PhysAddr,
        boot_stack_top: VirtAddr,
        entry: APEntry,
    ) {
        let apic_id = cpu_id as _;
        let lapic = super::imp::apic::local_apic();

        super::imp::ap::setup_ap_start_page(
            cpu_id,
            page_table_root,
            boot_stack_top,
            VirtAddr::from_usize(entry as usize),
        );

        unsafe { lapic.send_init_ipi(apic_id) };
        crate::time::spin_wait_for(Duration::from_millis(10)); // 10ms
        unsafe { lapic.send_sipi(super::imp::ap::AP_START_PAGE_INDEX, apic_id) };
        crate::time::spin_wait_for(Duration::from_micros(200)); // 200us
        unsafe { lapic.send_sipi(super::imp::ap::AP_START_PAGE_INDEX, apic_id) };
    }

    fn shutdown(_reason: ShutdownReason) -> ! {
        unsafe {
            const POWEROFF_PORT: u16 = 0x604;
            const POWEROFF_VALUE: u16 = 0x2000;
            core::arch::asm!(
                "outw %ax, (%dx)",
                in("ax") POWEROFF_VALUE,
                in("dx") POWEROFF_PORT,
                options(att_syntax, nomem, nostack, preserves_flags),
            );

            loop {
                core::arch::asm!("hlt", options(nomem, nostack));
            }
        }
    }
}
