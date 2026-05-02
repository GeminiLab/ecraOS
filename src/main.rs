#![no_std]
#![no_main]

use core::arch::global_asm;

mod plat_multiboot;

#[unsafe(no_mangle)]
fn main() {
    unsafe {
        core::arch::asm!(
            "
    mov     $0x3F8, %dx                     # The serial port COM1
    mov     $message, %esi                  # The message to print
    mov     $(message_end - message), %ecx  # The length of the message

    rep outsb

    mov     $0x604, %dx
    mov     $0x2000, %ax
    out     %ax, (%dx)

    hlt",
            options(att_syntax)
        );
    }
}

global_asm!(
    r#"
.section .rodata.boot
.type message, @object
message:
    .asciz  "\nHello, EcraOS!\n"
message_end:
"#,
    options(att_syntax)
);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
