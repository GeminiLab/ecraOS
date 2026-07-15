//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`exboot`]
//! and `ecraos-loader`.

#![no_std]
#![no_main]
#![deny(unfulfilled_lint_expectations)]

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

use log::{error, info};

mod device;
mod logging;
mod mem;
mod mp;
mod percpu;

macro_rules! kprintln {
    ($($arg:tt)*) => {
        exarch::dbcn_println!($($arg)*)
    };
}

pub(crate) use kprintln;

/// Banner line printed at startup.
const HELLO_ECRAOS: &str = "Hello, ecraOS!";
/// Attribution line printed at startup.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";
/// Horizontal line printed at startup.
const HLINE: &str = "------------------------------------------------------------";

/// Kernel entry called by specified boot modules through
/// [`call_kernel_entry`](exboot::call_kernel_entry).
///
/// For execution environment requirements when calling this function, see
/// [`call_kernel_entry`](exboot::call_kernel_entry) also.
///
/// # Safety
///
/// This function is the kernel entry and should only be called by the
/// bootloader. The bootloader should guarantee that the argument is valid.
///
/// This function should never be called directly.
#[exboot::kernel_entry]
pub unsafe fn kernel_entry(hart_id: usize, arg: *const exboot::BootArg) -> ! {
    // Relocate the kernel image.
    unsafe { mem::reloc::relocate_me() };

    // Clear the BSS.
    mem::clear_bss();

    // Initialize the per-CPU data area using the early slot. It maybe used while initializing
    // interrupts and timers.
    percpu::init_bsp_early();

    // Initialize the CPU ID for the BSP.
    mp::init_cpu_id_bsp(hart_id);

    // SAFETY: The bootloader guarantees that the argument is valid.
    let arg_ref = unsafe { arg.as_ref_unchecked() };

    // Perform early platform initialization.
    exarch::init::init_early(arg_ref.plat_arg);

    kprintln!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}\n");
    kprintln!(
        "Kernel entry on BSP(hart_id: {:#x}), arg: {:x?}\n",
        hart_id,
        arg_ref
    );

    mem::init_and_enable_vmm(kernel_entry_with_vmm as *const _, hart_id, arg)
}

/// The later kernel entry function that runs after the VMM is initialized.
///
/// # Safety
///
/// This function should only be called by the [`kernel_entry`] function, via
/// [`mem::init_vmm`], and should never be called directly.
pub unsafe fn kernel_entry_with_vmm(hart_id: usize, arg: *const exboot::BootArg) -> ! {
    unsafe { mem::reloc::relocate_me() };

    // Re-initialize the per-CPU data area pointer after relocation.
    percpu::init_bsp_after_reloc();

    // Call the relocation hook.
    exarch::reloc_hook::after_reloc();

    kprintln!(
        "{HLINE}\necraOS now in VMM world...\n{HLINE}\nVMM enabled on hart_id: {:#x}\n",
        hart_id
    );

    // Initialize logging.
    logging::init();

    info!("Logger initialized");

    // Finish memory initialization after enabling VMM.
    mem::init_after_enable_vmm();

    // Perform later platform initialization.
    let boot_arg = unsafe { arg.as_ref_unchecked() };
    exarch::init::init_later(boot_arg.plat_arg);

    // Probe devices.
    device::probe_devices(boot_arg.plat_arg);

    // Are we in the right place?
    let rsp: usize;
    let rip: usize;

    unsafe {
        core::arch::asm!(
            "lea {rip}, [rip]",
            "mov {rsp}, rsp",
            rip = out(reg) rip,
            rsp = out(reg) rsp,
            options(nomem, preserves_flags),
        );
    }

    kprintln!("rip: {:#x}, rsp: {:#x}", rip, rsp);

    // Is IDT correct now?
    unsafe { core::arch::asm!("int3", options(att_syntax)) }

    // Is GDT correct now?
    unsafe {
        let cs: usize;
        let rip: usize;

        core::arch::asm!(
            "mov %cs, {0}",
            "pushq {0}",
            "leaq 2f(%rip), {1}",
            "pushq {1}",
            "lretq",
            "2:",
            out(reg) cs,
            out(reg) rip,
            options(att_syntax),
        );

        kprintln!("cs: {:#x}, rip: {:#x}", cs, rip);
    }

    kprintln!("\n\nHere we go!\n\n");

    exarch::init::init_later(boot_arg.plat_arg);

    info!("Starting up secondary CPUs...");
    mp::start_secondary_cpus();

    // mem::remove_identical_mappings();

    info!("Timer: 0");
    let start = exarch::time::monotonic_time();
    for sec in 1..=12 {
        exarch::time::spin_wait_until(start + exarch::time::Duration::from_secs(sec));
        info!("Timer: {sec}");
    }

    exarch::power::poweroff()
}

/// The kernel entry function for the AP.
///
/// # Safety
///
/// This function should never be called directly.
pub unsafe fn kernel_entry_ap(phys_id: usize) -> ! {
    let cpu_id = mp::phys_to_logi_id(phys_id);

    // Mark the AP as up.
    mp::mark_ap_up(cpu_id);

    // Initialize the per-CPU data area for the AP.
    percpu::init_ap(cpu_id);

    // Initialize the CPU ID for the AP.
    mp::init_cpu_id_ap(phys_id);

    info!(
        "Kernel entry on AP {}(hart_id: {})",
        mp::current_cpu_id(),
        phys_id
    );

    // Perform early platform initialization for the AP.
    exarch::init::init_early_ap();

    // Is IDT correct now?
    unsafe { core::arch::asm!("int3", options(att_syntax)) }

    // Spin forever.
    loop {
        core::hint::spin_loop();
    }
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if logging::is_inited() {
        error!("Kernel panic: {}", info);
    } else {
        kprintln!("Kernel panic: {}", info);
    }

    exarch::power::poweroff()
}
