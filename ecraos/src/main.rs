//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`ecraldr_base`]
//! and `ecraldr`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![deny(unfulfilled_lint_expectations)]

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

use log::{error, info};

mod device;
mod irq;
mod kernel_if;
mod logging;
mod mem;
mod mp;
mod percpu;
mod task;
mod timer;

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

/// Runs the per-CPU task sleep and join smoke workload.
///
/// Four tasks each sleep for one second four times. A fifth task joins all four workers before
/// the per-CPU root task performs the BSP or AP terminal transition.
fn run_sleep_join_workload(is_bsp: bool) -> ! {
    const WORKER_COUNT: usize = 4;
    const SLEEP_ROUNDS: usize = 4;

    let workers: [task::JoinHandle; WORKER_COUNT] = core::array::from_fn(|worker_index| {
        task::spawn(move || {
            for round in 1..=SLEEP_ROUNDS {
                task::sleep(exarch::time::Duration::from_secs(1));
                info!(
                    "Sleep task {worker_index} on CPU {} completed round {round}/{SLEEP_ROUNDS}",
                    mp::current_cpu_id()
                );
            }
        })
    });

    let mut joiner = task::spawn(move || {
        for (worker_index, mut worker) in workers.into_iter().enumerate() {
            let _ = worker.join();
            info!(
                "Join task on CPU {} joined sleep task {worker_index}/{WORKER_COUNT}",
                mp::current_cpu_id()
            );
        }
        info!(
            "Join task on CPU {} joined all {WORKER_COUNT} sleep tasks",
            mp::current_cpu_id()
        );
    });

    let _ = joiner.join();

    if is_bsp {
        exarch::power::poweroff()
    } else {
        task::run_idle()
    }
}

fn print_hello_banner() {
    kprintln!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}");
    kprintln!(
        "Target triple: {}\nHost triple  : {}\n{HLINE}\n",
        target_triple::TARGET,
        target_triple::HOST
    );
}

/// Kernel entry called by specified boot modules through
/// [`call_kernel_entry`](ecraldr_base::call_kernel_entry).
///
/// For execution environment requirements when calling this function, see
/// [`call_kernel_entry`](ecraldr_base::call_kernel_entry) also.
///
/// # Safety
///
/// This function is the kernel entry and should only be called by the
/// bootloader. The bootloader should guarantee that the argument is valid.
///
/// This function should never be called directly.
#[ecraldr_base::kernel_entry]
pub unsafe fn kernel_entry(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
    // Relocate the kernel image.
    unsafe { mem::reloc::relocate_me() };

    // Clear the BSS.
    mem::sections::clear_bss();

    // Initialize the per-CPU data area using the early slot. It maybe used while initializing
    // interrupts and timers.
    percpu::init_bsp_early();

    // Initialize the CPU ID for the BSP.
    mp::init_cpu_id_bsp(hart_id);

    // SAFETY: The bootloader guarantees that the argument is valid.
    let arg_ref = unsafe { arg.as_ref_unchecked() };

    // Perform early platform initialization.
    exarch::init::init_early(arg_ref.plat_arg);

    // Print the hello banner.
    print_hello_banner();
    kprintln!(
        "Kernel entry on BSP(hart_id: {:#x}), arg: {:x?}\n",
        hart_id,
        arg_ref
    );

    // Enable the VMM, and jump to the upper address space.
    mem::init_and_enable_vmm(kernel_entry_with_vmm as *const _, hart_id, arg)
}

/// The later kernel entry function that runs after the VMM is initialized.
///
/// # Safety
///
/// This function should only be called by the [`kernel_entry`] function, via
/// [`mem::init_vmm`], and should never be called directly.
pub unsafe fn kernel_entry_with_vmm(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
    // Relocate the kernel image again.
    unsafe { mem::reloc::relocate_me() };

    // Re-initialize the per-CPU data area pointer after relocation, to fix the per-CPU data area
    // pointer after relocation.
    percpu::init_bsp_after_reloc();

    // Call the relocation hook.
    exarch::reloc_hook::after_reloc();

    // Print the hello banner.
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
    exarch::init::init_later();

    // Probe devices.
    device::probe_devices(boot_arg.plat_arg);

    irq::init();

    timer::init_bsp();

    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());

    kprintln!("\n\nHere we go!\n\n");

    info!("Starting up secondary CPUs...");
    mp::start_secondary_cpus().expect("failed to start secondary CPUs");

    mem::remove_identical_mappings();

    run_sleep_join_workload(true)
}

/// The kernel entry function for the AP.
///
/// # Safety
///
/// This function should never be called directly.
pub unsafe fn kernel_entry_ap(phys_id: usize) -> ! {
    let cpu_id = mp::phys_to_logi_id(phys_id);

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

    // Initialize memory for the AP.
    mem::init_ap();

    exarch::init::init_later_ap();

    timer::init_ap();

    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());

    mp::mark_ap_up(cpu_id);

    run_sleep_join_workload(false)
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use exarch::power::{ShutdownReason, shutdown};

    if logging::is_inited() {
        error!("Kernel panic: {}", info);
    } else {
        kprintln!("Kernel panic: {}", info);
    }

    shutdown(ShutdownReason::Panicked)
}
