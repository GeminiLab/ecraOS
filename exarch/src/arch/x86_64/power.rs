//! Power-off via the QEMU ACPI PM control port (`0x604`).

use core::time::Duration;

use memory_addr::{PhysAddr, VirtAddr};

use crate::power::{APEntry, CpuStartError, PhysicalCpuId, ShutdownReason};

/// Starts an x86-64 application processor.
pub fn cpu_up(
    cpu_id: PhysicalCpuId,
    page_table_root: PhysAddr,
    boot_stack_top: VirtAddr,
    entry: APEntry,
) -> Result<(), CpuStartError> {
    let apic_id = u32::try_from(cpu_id).map_err(|_| CpuStartError::InvalidCpu)?;
    super::imp::ap::setup_ap_start_page(
        cpu_id,
        page_table_root,
        boot_stack_top,
        VirtAddr::from_usize(entry as usize),
    );

    unsafe { super::imp::apic::send_init_ipi(apic_id) };
    crate::time::spin_wait_for(Duration::from_millis(10)); // 10ms
    unsafe { super::imp::apic::send_sipi(super::imp::ap::AP_START_PAGE_INDEX, apic_id) };
    crate::time::spin_wait_for(Duration::from_micros(200)); // 200us
    unsafe { super::imp::apic::send_sipi(super::imp::ap::AP_START_PAGE_INDEX, apic_id) };
    Ok(())
}

/// Shuts down the x86-64 platform.
pub fn shutdown(_reason: ShutdownReason) -> ! {
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
