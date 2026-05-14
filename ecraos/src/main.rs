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

mod mem;

macro_rules! early_println {
    ($($arg:tt)*) => {
        explat::dbcn_println!($($arg)*);
    };
}

pub(crate) use early_println;

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
pub fn kernel_entry(hart_id: usize, arg: *const exboot::ExbootArg) -> ! {
    unsafe { mem::reloc::relocate_me() };
    mem::clear_bss();

    // SAFETY: The bootloader guarantees that the argument is valid.
    let arg = unsafe { arg.as_ref_unchecked() };

    let init_result = explat::init::init_early(arg.plat_arg);

    early_println!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}\n");
    early_println!("Kernel entry on hart_id: {:#x}, arg: {:x?}\n", hart_id, arg);

    mem::init_vmm(init_result.memory_info, arg.boot_stack);

    early_println!("\n\nHello, vmm!\n\n");

    // `init_vmm` removed the identity map for RAM and reloaded CR3 to flush the
    // TLB, so a load at a fixed low VA (e.g. `0x205000`) would now #PF:
    // `unsafe { core::ptr::read_volatile(0x205000 as *const u8) };`

    let low_va = memory_addr::VirtAddr::from_usize(0x205000usize + 0xffff8000_00000000);
    early_println!("Trying to access low VA: {:#x}", low_va);
    let low_ptr = low_va.as_ptr();
    let u: u8 = unsafe { low_ptr.read() };
    early_println!("Read value @ {:p}: {:#x}", low_ptr, u);

    explat::power::poweroff()
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    early_println!("Kernel panic: {}", info);
    explat::power::poweroff()
}
