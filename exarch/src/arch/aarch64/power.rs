//! PSCI-based AArch64 CPU startup and shutdown.

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, Ordering},
};

use memory_addr::{PhysAddr, VirtAddr, va};

use crate::power::{APEntry, CpuStartError, PhysicalCpuId, ShutdownReason};

const PSCI_CPU_ON: u64 = 0xC400_0003;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;

/// Indicates whether PSCI uses the HVC instruction.
///
/// This is expected to be set by the platform-specific probe function. If not set, SMC is used as
/// it's more likely to be supported.
static PSCI_USES_HVC: AtomicBool = AtomicBool::new(false);

/// Selects the PSCI HVC conduit when advertised by firmware.
pub fn set_psci_method(hvc: bool) {
    PSCI_USES_HVC.store(hvc, Ordering::Release);
}

macro_rules! psci_with {
    ($instr: literal, $function: expr, $arg0: expr, $arg1: expr, $arg2: expr) => {
        {
            let mut value = $function;
            unsafe {
                asm!(concat!($instr, " #0"), inout("x0") value, in("x1") $arg0, in("x2") $arg1, in("x3") $arg2,
                    options(nomem, preserves_flags));
            }
            value as i64
        }
    };
}

/// Invokes the PSCI function using the appropriate instruction.
pub fn invoke(function: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    if PSCI_USES_HVC.load(Ordering::Acquire) {
        psci_with!("hvc", function, arg0, arg1, arg2)
    } else {
        psci_with!("smc", function, arg0, arg1, arg2)
    }
}

/// Requests PSCI to start one application CPU.
pub fn cpu_up(
    phys_id: PhysicalCpuId,
    _page_table_root: PhysAddr,
    boot_stack_top: VirtAddr,
    entry: APEntry,
) -> Result<(), CpuStartError> {
    let entry_pa = crate::kernel_if::virt_to_phys(va!(entry as *const () as usize));
    let stack_pa = crate::kernel_if::virt_to_phys(boot_stack_top);
    let result = invoke(
        PSCI_CPU_ON,
        phys_id as u64,
        entry_pa.as_usize() as u64,
        stack_pa.as_usize() as u64,
    );
    match result {
        0 => Ok(()),
        -1 => Err(CpuStartError::Unsupported),
        -2 => Err(CpuStartError::InvalidCpu),
        -3 => Err(CpuStartError::FirmwareDenied),
        -4 => Err(CpuStartError::InvalidState),
        _ => Err(CpuStartError::Transport),
    }
}

/// Requests a PSCI system shutdown and spins if firmware returns.
pub fn shutdown(_reason: ShutdownReason) -> ! {
    let _ = invoke(PSCI_SYSTEM_OFF, 0, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}
