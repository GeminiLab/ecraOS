use core::arch::global_asm;

global_asm!(
    r#"
.equ    MULTIBOOT_MAGIC,    0x1BADB002
.equ    MULTIBOOT_FLAGS,    0x00010002  # bit 1 (meminfo) and bit 16 (load address header fields)
.equ    MULTIBOOT_CHECKSUM, -(MULTIBOOT_MAGIC + MULTIBOOT_FLAGS)

# The entry point of the kernel
.section .text.boot
.code32
.global _start
_start:
    mov     %eax, %edi      # The multiboot magic number
    mov     %ebx, %esi      # The multiboot information structure
    jmp     entry32

.balign 4
.type multiboot_header, @object
multiboot_header:
    .long   MULTIBOOT_MAGIC         # The magic number
    .long   MULTIBOOT_FLAGS         # The flags
    .long   MULTIBOOT_CHECKSUM      # The checksum
    .long   multiboot_header        # The header address, the linear address where the magic number should be loaded
    .long   _start                  # The start address of the kernel image, same as _start in this demo
    .long   0                       # The end address of the data segment, 0 means the end of the kernel image
    .long   0                       # The end address of the bss segment, 0 means no bss segment
    .long   _start                  # The entry point of the kernel
    # There may be other fields here if we set the bit 2 of the flags, we can safely ignore them here

.global entry32
entry32:
    jmp main
    "#,
    options(att_syntax)
);
