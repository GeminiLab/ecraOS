//! ecraOS loaders common part.

#![no_std]
#![no_main]
#![feature(used_with_arg)] // Used when including the kernel.

use ecraldr_base::BOOTSTACK_SIZE;

/// The boot stack.
///
/// Note that the bootstack **IS NOT** a part of the bss section. Because the
/// bss sections of the loader overlaps with the kernel's bss section, therefore
/// placing the bootstack in the bss section may cause corruption of data or
/// stack.
#[unsafe(link_section = ".boot_stack")]
#[used(compiler)]
#[used(linker)]
static BOOTSTACK: [u8; BOOTSTACK_SIZE] = [0; BOOTSTACK_SIZE];

// Include the kernel.
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));

#[macro_export]
macro_rules! ecraldr_panic_handler {
    () => {
        /// Loader stage panic handler.
        ///
        /// This handler is called when a panic occurs during the loader stage. The
        /// kernel has its own panic handler.
        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo) -> ! {
            loop {}
        }
    };
}
