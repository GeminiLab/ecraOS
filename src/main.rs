#![no_std]
#![no_main]

use explat_x86_64_multiboot as _;

const HELLO_MESSAGE: &str = "\n\nHello, EcraOS!\nA Derivative of the ArceOS project.\n";

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    explat::init::init_early();
    explat::debug_console::write_str(HELLO_MESSAGE);
    explat::power::poweroff()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
