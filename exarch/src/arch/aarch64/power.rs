//! PSCI-based AArch64 CPU startup and shutdown.

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, Ordering},
};

use memory_addr::{PhysAddr, VirtAddr, va};

use crate::{
    kernel_if::virt_to_phys,
    power::{APEntry, CpuStartError, PhysicalCpuId, ShutdownReason},
};

core::arch::global_asm!(include_str!("ap_start.S"));

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
pub fn invoke_psci(function: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    if PSCI_USES_HVC.load(Ordering::Acquire) {
        psci_with!("hvc", function, arg0, arg1, arg2)
    } else {
        psci_with!("smc", function, arg0, arg1, arg2)
    }
}

/// The arguments passed to the AP trampoline.
///
/// This structure is placed on the AP boot stack and consumed by the assembly trampoline before
/// transferring control to the Rust entry point.
#[repr(C)]
struct ApBootArgs {
    /// The physical ID of the CPU being started.
    phys_id: PhysicalCpuId,
    /// The physical address of the shared translation-table root.
    page_table_root: PhysAddr,
    /// The high-half virtual address of the AP stack top.
    stack_top: VirtAddr,
    /// The high-half virtual address of the Rust AP entry.
    entry: VirtAddr,
}

unsafe extern "C" {
    fn _start_ap();
}

/// Requests PSCI to start one application CPU.
pub fn cpu_up(
    phys_id: PhysicalCpuId,
    page_table_root: PhysAddr,
    boot_stack_top: VirtAddr,
    entry: APEntry,
) -> Result<(), CpuStartError> {
    // Write the AP boot arguments to the top of the AP boot stack.
    let args_addr = VirtAddr::from_usize(
        boot_stack_top
            .as_usize()
            .checked_sub(core::mem::size_of::<ApBootArgs>())
            .expect("AP boot stack is too small for startup arguments"),
    );
    let args = args_addr.as_mut_ptr_of::<ApBootArgs>();
    // SAFETY: The AP boot stack is exclusively owned by the CPU being started,
    // and the trampoline consumes these fields before entering Rust.
    unsafe {
        core::ptr::write_volatile(
            args,
            ApBootArgs {
                phys_id,
                page_table_root,
                stack_top: boot_stack_top,
                entry: VirtAddr::from_ptr_of(entry as *const ()),
            },
        );
    }

    // Convert the trampoline and arguments to physical addresses for PSCI.
    let trampoline_pa = virt_to_phys(va!(_start_ap as *const () as usize));
    let args_pa = virt_to_phys(args_addr);

    // Start the AP by PSCI.
    let result = invoke_psci(
        PSCI_CPU_ON,
        phys_id as u64,
        trampoline_pa.as_usize() as u64,
        args_pa.as_usize() as u64,
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
    let _ = invoke_psci(PSCI_SYSTEM_OFF, 0, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}
