//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`exboot`]
//! and `ecraos-loader`.

#![no_std]
#![no_main]

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

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

mod mem;

macro_rules! kprintln {
    ($($arg:tt)*) => {
        explat::dbcn_println!($($arg)*)
    };
}

pub(crate) use kprintln;

use crate::mem::vmm;

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
    unsafe { mem::reloc::relocate_me() };
    mem::clear_bss();

    // SAFETY: The bootloader guarantees that the argument is valid.
    let arg_ref = unsafe { arg.as_ref_unchecked() };

    explat::init::init_early(arg_ref.plat_arg);

    kprintln!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}\n");
    kprintln!(
        "Kernel entry on hart_id: {:#x}, arg: {:x?}\n",
        hart_id,
        arg_ref
    );

    mem::enable_vmm(kernel_entry_with_vmm as *const _, hart_id, arg)
}

/// The later kernel entry function that runs after the VMM is initialized.
///
/// # Safety
///
/// This function should only be called by the [`kernel_entry`] function, via
/// [`mem::init_vmm`], and should never be called directly.
pub unsafe fn kernel_entry_with_vmm(hart_id: usize, _arg: *const exboot::BootArg) -> ! {
    unsafe { mem::reloc::relocate_me() };

    kprintln!("VMM enabled on hart_id: {:#x}\n", hart_id);

    mem::after_enable_vmm();

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
    kprintln!("\n\nHere we go!\n\n");

    explat::power::poweroff()
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kprintln!("Kernel panic: {}", info);
    explat::power::poweroff()
}
