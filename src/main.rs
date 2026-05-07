//! `ecraOS`` kernel binary.

#![no_std]
#![no_main]

/// Banner line printed at startup.
const HELLO_ECRAOS: &str = "Hello, ecraOS!";
/// Attribution line printed at startup.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";

/// This is essential to link the concrete platform implementation.
///
/// `link_with_explat_impl` is a custom rustc config that will be set when
/// building the kernel with a concrete platform implementation (by rustc flag
/// `--extern explat_impl=<path-to-impl-lib>`).
///
/// This flag does not need to be set when checking code with `cargo check`,
/// which means that `rust-analyzer` works perfectly fine when this is not
/// present.
#[cfg(link_with_explat_impl)]
extern crate explat_impl;

/// Kernel entry after the platform has set up long mode, identity mapping, and
/// a boot stack.
///
/// `hart_id` is the hardware thread id (bootstrap processor is `0` here). `arg`
/// is the platform-specific value forwarded to
/// [`explat::init::InitIf::init_early`].
#[explat::kernel_entry]
pub fn kernel_entry(hart_id: usize, arg: usize) -> ! {
    let init_result = explat::init::init_early(arg);
    explat::dbcn_println!("\n\n{}\n\n{}\n", HELLO_ECRAOS, DISCLAIMER);
    explat::dbcn_println!("Kernel entry on hart_id: {:#x}, arg: {:#x}", hart_id, arg);

    for memory_region in init_result.memory_regions {
        explat::dbcn_println!(
            "Memory region: {:#x} - {:#x}, {:.2} MiB",
            memory_region.start,
            memory_region.start + memory_region.size,
            memory_region.size as f64 / 1048576.0,
        );
    }

    explat::power::poweroff()
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
