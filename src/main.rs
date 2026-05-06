//! `ecraOS`` kernel binary.

#![no_std]
#![no_main]

use explat_x86_64_multiboot as _;

/// Banner line printed at startup.
const HELLO_ECRAOS: &str = "Hello, ecraOS!";
/// Attribution line printed at startup.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";

/// Kernel entry after the platform has set up long mode, identity mapping, and a boot stack.
///
/// `hart_id` is the hardware thread id (bootstrap processor is `0` here). `arg` is the
/// platform-specific value forwarded to [`explat::init::InitIf::init_early`].
#[explat::kernel_entry]
pub fn kernel_entry(hart_id: usize, arg: usize) -> ! {
    explat::init::init_early(arg);
    explat::dbcn_println!("\n\n{}\n\n{}\n", HELLO_ECRAOS, DISCLAIMER);
    explat::dbcn_println!("Kernel entry on hart_id: {:#x}, arg: {:#x}", hart_id, arg);
    explat::power::poweroff()
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
