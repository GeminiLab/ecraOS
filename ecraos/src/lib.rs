//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`ecraldr_base`]
//! and `ecraldr`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![feature(extern_item_impls)]
#![feature(doc_cfg)]
#![deny(unfulfilled_lint_expectations)]

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

pub use expanic::{bug, kpanic};
use log::info;

mod device;
#[cfg(any(feature = "foundation-test", doc))]
pub mod foundation_test;
mod ipi;
mod kernel_if;
mod logging;
mod mem;
pub mod mp;
pub mod panic;
mod percpu;
pub mod task;
mod test_guard;
mod timer;
/// Semantic cross-CPU translation invalidation.
///
/// Mapping transactions reuse the common acknowledged IPI protocol.
mod tlb;
mod trap;

#[doc(hidden)]
pub mod reexport {
    pub use exarch;
}

#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {
        crate::reexport::exarch::dbcn_println!($($arg)*)
    };
}

/// The ASCII ecraOS wordmark.
///
/// Uses seven-row lowercase letters and ten-row uppercase letters on a shared baseline, centered
/// in the 78-column interior of the banner table.
///
/// ASCII art by GPT-6.1-Sol, refined by the ecraOS author.
const LOGO: &str = r"|                                                                              |
|                                                .ooooooo.        .oooooo.     |
|                                              .ooooooooooo.    .oooooooo.     |
|                                             .ooo.     .ooo.   ooo.           |
|      .ooooo     ooooo.  .oo oo.   .oooooo  .ooo        oooo   oooo           |
|     ooo..ooo   ooo..o.  .ooooo   ooo..ooo  ooo.        oooo   .oooooo.       |
|    ooo. .oo.  ooo       ooo.    ooo   ooo  ooo.        ooo.     .oooooo      |
|    ooooooo.  .oo.       ooo    .oo.  .oo.  oooo       .ooo         .ooo      |
|    ooo       .oo.       ooo    .oo.  ooo.  .ooo.     .ooo.   ..    .ooo      |
|    oooo..oo   ooo..oo  .oo.    .ooo.oooo    .ooooooooooo.   .ooooooooo       |
|     .ooooo.    ooooo.  .oo      oooo..oo      oooooooo.      .oooooo.        |
|                                                                              |";

/// The attribution line printed at startup.
///
/// Identifies the ArceOS project from which ecraOS is derived.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";

/// The ArceOS organization URL.
///
/// Links to the upstream organization credited in the startup banner.
const ARCEOS_REPO_LINK: &str = "https://github.com/arceos-org/";

/// The ecraOS repository URL.
///
/// Links to the project's source code from the startup banner.
const ECRAOS_REPO_LINK: &str = "https://github.com/GeminiLab/ecraOS/";

/// The horizontal separator for startup messages.
///
/// Spans 80 columns.
const HLINE: &str =
    "--------------------------------------------------------------------------------";

/// The horizontal border of the startup banner table.
///
/// Spans 80 columns, including the corner characters around the 78-column cell interior.
const BANNER_BORDER: &str =
    "+------------------------------------------------------------------------------+";

/// Prints the startup banner.
///
/// Writes an 80-column table through the early console before logging is initialized. The first
/// cell contains the centered wordmark and attribution. The second lists project links, and the
/// third contains the target and host triples.
fn print_hello_banner() {
    kprintln!();
    kprintln!("{BANNER_BORDER}");
    kprintln!("{LOGO}");
    kprintln!("| {:76} |", "");
    kprintln!("| {DISCLAIMER:^76.76} |");
    kprintln!("{BANNER_BORDER}");
    kprintln!("| ArceOS org   : {:61.61} |", ARCEOS_REPO_LINK);
    kprintln!("| ecraOS repo  : {:61.61} |", ECRAOS_REPO_LINK);
    kprintln!("{BANNER_BORDER}");
    kprintln!("| Target triple: {:61.61} |", target_triple::TARGET);
    kprintln!("| Host triple  : {:61.61} |", target_triple::HOST);
    kprintln!("{BANNER_BORDER}");
    kprintln!();
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
unsafe fn kernel_entry(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
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
        "Kernel entry on BSP(hart_id: {:#x}), arg: {:#x?}\n",
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
/// [`mem::init_and_enable_vmm`], and should never be called directly.
unsafe fn kernel_entry_with_vmm(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
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

    trap::init();
    ipi::init();
    tlb::init();
    exarch::ipi::init_percpu();
    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());
    timer::init_bsp();
    mp::mark_current_cpu_ipi_ready();

    kprintln!("\n\nHere we go!\n\n");

    info!("Starting up secondary CPUs...");
    mp::start_secondary_cpus().expect("failed to start secondary CPUs");

    mem::remove_identical_mappings();
    mp::publish_boot_mapping_completion();

    app_main_bsp();

    exarch::power::poweroff()
}

#[eii]
pub fn app_main_bsp() {}

#[eii]
pub fn app_main_ap() {
    task::run_idle()
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

    // Perform later platform initialization for the AP.
    exarch::init::init_later_ap();

    // Enable the local source only after the BSP has installed common dispatch and mailboxes.
    exarch::ipi::init_percpu();
    // Initialize the scheduler for the AP.
    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());
    timer::init_ap();
    mp::mark_current_cpu_ipi_ready();
    mp::mark_ap_up(cpu_id);

    // Online APs keep servicing IPIs here until boot has finished all mapping changes and joins.
    mp::wait_boot_mapping_completion();

    app_main_ap();

    task::run_idle()
}
