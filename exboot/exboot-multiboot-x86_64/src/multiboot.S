.equ    MULTIBOOT_MAGIC,    0x1BADB002
.equ    MULTIBOOT_FLAGS,    0x00010002  # bit 1 (meminfo) and bit 16 (load address header fields)
.equ    MULTIBOOT_CHECKSUM, -(MULTIBOOT_MAGIC + MULTIBOOT_FLAGS)

# The entry point of the kernel
.section .text.boot
.code32
.global _start
_start:
    # Save multiboot arguments
    movl    %eax, %edi
    movl    %ebx, %esi

    # Jump over multiboot header
    jmp     entry32

.balign 4
multiboot_header:
    .long   MULTIBOOT_MAGIC         # Magic number
    .long   MULTIBOOT_FLAGS         # Flags
    .long   MULTIBOOT_CHECKSUM      # Checksum
    .long   multiboot_header        # Header address
    .long   _sloader                # Load start
    .long   _edata                  # Load end
    .long   _ebss                   # BSS end
    .long   _start                  # Entry point

.code32
.global entry32
entry32:
    # Disable interrupts
    cli

    # Initialize GDT
    lgdt    gdt_desc

    # Initialize data segments
    movw    $0x18, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %fs
    movw    %ax, %gs
    movw    %ax, %ss

    # Enable PAE, PGE in CR4
    movl    ${cr4}, %eax
    movl    %eax, %cr4

    # Load the boot pagetable
    lea     boot_pt_pml4, %eax
    movl    %eax, %cr3

    # Enable LME, NXE in EFER
    movl    $0xC0000080, %ecx
    xorl    %edx, %edx
    movl    ${efer}, %eax
    wrmsr

    # Enable PE, MP, NE, WP, PG in CR0
    movl    ${cr0}, %eax
    movl    %eax, %cr0

    # Jump to long mode
    ljmp    $0x10, $entry64

.code64
entry64:
    # Clear data segments
    xorq    %rax, %rax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %fs
    movw    %ax, %gs
    movw    %ax, %ss

    # Set up the boot stack
    lea     _ebootstack, %rsp

    call    rust_entry64_bsp

0:
    hlt
    jmp     0b

# The init GDT
.section .rodata
.balign 8
gdt_desc:
    .word   gdt_end - gdt - 1   # GDT limit
    .long   gdt                 # GDT base

.balign 16
gdt:
    .quad 0x0000000000000000    # 0x00: null
    .quad 0x00cf9b000000ffff    # 0x08: code segment (base=0, limit=0xfffff, type=32bit code exec/read, DPL=0, 4k)
    .quad 0x00af9b000000ffff    # 0x10: code segment (base=0, limit=0xfffff, type=64bit code exec/read, DPL=0, 4k)
    .quad 0x00cf93000000ffff    # 0x18: data segment (base=0, limit=0xfffff, type=32bit data read/write, DPL=0, 4k)
gdt_end:

# The boot pagetable
.section .rodata
.balign 4096
boot_pt_pml4:
    # 0x0000_0000 ~ 0x7f_ffff_ffff (512 GiB)
    .quad boot_pt_pdpt + 0x3    # PRESENT | WRITABLE | paddr(boot_pt_pdpt)
    .fill 511, 8, 0

.balign 4096
boot_pt_pdpt:
.set i, 0
.rept 512
    .quad 0x40000000 * i | 0x83 # PRESENT | WRITABLE | HUGE_PAGE | paddr(0x4000_0000 * i)
    .set i, i + 1
.endr
