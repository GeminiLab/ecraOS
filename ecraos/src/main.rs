//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`exboot`]
//! and `ecraos-loader`.

#![no_std]
#![no_main]

use core::fmt;

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

mod mem;
mod reloc;

/// Banner line printed at startup.
const HELLO_ECRAOS: &str = "Hello, ecraOS!";
/// Attribution line printed at startup.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";
/// Horizontal line printed at startup.
const HLINE: &str = "------------------------------------------------------------";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AutoSize(pub usize);

impl fmt::Display for AutoSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = self.0;

        if size < 1000 {
            write!(f, "{size:>6}   B")
        } else if size < 1000 * 1024 {
            let size_kib = size as f64 / 1024.0;
            write!(f, "{size_kib:>6.2} KiB")
        } else if size < 1000 * 1024 * 1024 {
            let size_mib = size as f64 / 1024.0 / 1024.0;
            write!(f, "{size_mib:>6.2} MiB")
        } else if size < 1000 * 1024 * 1024 * 1024 {
            let size_gib = size as f64 / 1024.0 / 1024.0 / 1024.0;
            write!(f, "{size_gib:>6.2} GiB")
        } else if size < 1000 * 1024 * 1024 * 1024 * 1024 {
            let size_tib = size as f64 / 1024.0 / 1024.0 / 1024.0 / 1024.0;
            write!(f, "{size_tib:>6.2} TiB")
        } else {
            let size_pib = size as f64 / 1024.0 / 1024.0 / 1024.0 / 1024.0 / 1024.0;
            write!(f, "{size_pib:>6.2} PiB")
        }
    }
}

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
    mem::init_mem_early(init_result.memory_info);

    explat::power::poweroff()
}

fn print_reloc_info() {
    explat::dbcn_println!(
        "Kernel relocated to: {:#x}",
        reloc::sections::kernel_range()
    );
    for (name, range) in reloc::sections::all_sections() {
        explat::dbcn_println!("  {:<10}: {:#x} ({})", name, range, AutoSize(range.size()),);
    }
    explat::dbcn_println!();
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    explat::dbcn_println!("Kernel panic: {}", info);
    explat::power::poweroff()
}
