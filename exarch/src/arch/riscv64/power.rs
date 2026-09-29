use core::ops::BitOr;

use expt::opaque::OpaquePageTableRoot;
use log::{error, info, warn};
use memory_addr::{VirtAddr, va};

use crate::power::{APEntry, CpuStartError, PhysicalCpuId, ShutdownReason};

core::arch::global_asm!(include_str!("mp.S"));

/// Starts a RISC-V application hart.
pub fn cpu_up(
    phys_id: PhysicalCpuId,
    page_table_root: OpaquePageTableRoot,
    boot_stack_top: VirtAddr,
    entry: APEntry,
) -> Result<(), CpuStartError> {
    let OpaquePageTableRoot::Single(page_table_root) = page_table_root;
    if sbi_rt::probe_extension(sbi_rt::Hsm).is_unavailable() {
        warn!("HSM SBI extension is not supported for current SEE.");
        return Err(CpuStartError::Unsupported);
    }

    unsafe extern "C" {
        fn _start_ap();
    }

    const AP_BOOT_ARG_SLOT_SIZE: usize = core::mem::size_of::<u64>();

    // `boot_stack_top` is one-past-end, so translate the last argument slot.
    let boot_stack_top_pa =
        crate::kernel_if::virt_to_phys(va!(boot_stack_top.as_usize() - AP_BOOT_ARG_SLOT_SIZE))
            + AP_BOOT_ARG_SLOT_SIZE;
    let start_ap_pa = crate::kernel_if::virt_to_phys(va!(_start_ap as *const () as _));

    let boot_stack_top_ptr = boot_stack_top.as_mut_ptr_of::<u64>();
    unsafe {
        *boot_stack_top_ptr.sub(1) = {
            let satp: u64;

            core::arch::asm!(
                "csrr {satp}, satp",
                satp = out(reg) satp,
                options(nomem, nostack),
            );

            satp.wrapping_shr(44)
                .wrapping_shl(44)
                .bitor((page_table_root.as_usize() as u64).wrapping_shr(12))
        };
        *boot_stack_top_ptr.sub(2) = boot_stack_top.as_usize() as _;
        *boot_stack_top_ptr.sub(3) = entry as *const () as _;
    }

    let result = sbi_rt::hart_start(
        phys_id,
        start_ap_pa.as_usize(),
        boot_stack_top_pa.as_usize(),
    );
    if result.is_ok() {
        return Ok(());
    }

    match result.error as isize {
        -2 => Err(CpuStartError::Unsupported),
        -3 => Err(CpuStartError::InvalidCpu),
        -4 => Err(CpuStartError::FirmwareDenied),
        _ => Err(CpuStartError::Transport),
    }
}

/// Shuts down the RISC-V platform.
pub fn shutdown(_reason: ShutdownReason) -> ! {
    info!("Shutting down...");
    sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason);
    error!("It should shutdown!");
    loop {
        core::hint::spin_loop();
    }
}
