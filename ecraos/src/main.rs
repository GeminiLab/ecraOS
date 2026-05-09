//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`exboot`]
//! and `ecraos-loader`.

#![no_std]
#![no_main]

use explat::init::EarlyMemoryRegions;

/// Include the concrete platform implementation.
///
/// `building_ecraos` is a custom rustc config that will be set when building
/// the kernel with a concrete platform implementation (by rustc flag
/// `--extern explat_impl=<path-to-impl-lib>`).
///
/// This flag does not need to be set when checking code with `cargo check`,
/// which means that `rust-analyzer` works perfectly fine when this is not
/// present.
#[cfg(building_ecraos)]
extern crate explat_impl;

mod reloc;

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
#[exboot::kernel_entry]
pub fn kernel_entry(hart_id: usize, arg: ::exboot::BootArg) -> ! {
    unsafe { reloc::relocate_me() };

    let init_result = explat::init::init_early(arg);

    explat::dbcn_println!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}\n");
    explat::dbcn_println!("Kernel entry on hart_id: {:#x}, arg: {:x?}\n", hart_id, arg);

    print_reloc_info();
    print_mem_info(&init_result.memory_regions);

    explat::power::poweroff()
}

fn print_reloc_info() {
    unsafe extern "C" {
        fn _skernel();
        fn _ekernel();
    }

    let kernel_start = _skernel as *const () as usize;
    let kernel_end = _ekernel as *const () as usize;

    explat::dbcn_println!("Relocation info:");
    explat::dbcn_println!("kernel: {:#x} - {:#x}", kernel_start, kernel_end);
    explat::dbcn_println!();
}

fn print_mem_info(regions: &EarlyMemoryRegions) {
    unsafe extern "C" {
        fn _stext();
        fn _etext();
        fn _srodata();
        fn _erodata();
        fn _sdata();
        fn _edata();
        fn _sbss();
        fn _ebss();
    }

    let text_start = _stext as *const () as usize;
    let text_end = _etext as *const () as usize;
    let rodata_start = _srodata as *const () as usize;
    let rodata_end = _erodata as *const () as usize;
    let data_start = _sdata as *const () as usize;
    let data_end = _edata as *const () as usize;
    let bss_start = _sbss as *const () as usize;
    let bss_end = _ebss as *const () as usize;

    explat::dbcn_println!("Memory info:");

    explat::dbcn_println!(" kernel sections:");
    explat::dbcn_println!("  .text   : {:#x} - {:#x}", text_start, text_end);
    explat::dbcn_println!("  .rodata : {:#x} - {:#x}", rodata_start, rodata_end);
    explat::dbcn_println!("  .data   : {:#x} - {:#x}", data_start, data_end);
    explat::dbcn_println!("  .bss    : {:#x} - {:#x}", bss_start, bss_end);

    explat::dbcn_println!(" early memory regions:");
    for region in regions {
        explat::dbcn_println!(
            "  {:<#010x} - {:<#010x}, {:.2} MiB",
            region.start,
            region.start + region.size,
            region.size as f64 / 1048576.0
        );
    }
    explat::dbcn_println!();
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    explat::dbcn_println!("Kernel panic: {}", info);
    explat::power::poweroff()
}
