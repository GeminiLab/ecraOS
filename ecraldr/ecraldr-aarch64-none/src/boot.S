.section .text.boot
.global _start
.type _start, %function

// AArch64 QEMU virt enters with x0=boot CPU ID and x1=DTB address.
_start:
    mov     x19, x0
    mov     x20, x1
    msr     daifset, #0xf
    ldr     x0, ={boot_stack_top}
    mov     sp, x0
    cbnz    x20, 0f
    mov     x21, #0x40000000
    mov     x22, #0x48000000
    mov     w23, #0xfeed
    movk    w23, #0xd00d, lsl #16
1:
    ldr     w0, [x21]
    rev     w0, w0
    cmp     w0, w23
    b.eq    2f
    add     x21, x21, #0x1000
    cmp     x21, x22
    b.lo    1b
    b       unsupported_el
2:
    mov     x20, x21
0:

    // Normalize EL2 into EL1h.  EL1 starts at the same path below.
    mrs     x21, CurrentEL
    lsr     x21, x21, #2
    cmp     x21, #1
    b.eq    el1_entry
    cmp     x21, #2
    b.eq    enter_el1
    b       unsupported_el

enter_el1:
    // EL1h with all asynchronous exceptions masked until the common trap
    // setup is installed.  HCR_EL2.RW selects AArch64 at EL1.
    mov     x0, #0x3c5
    msr     SPSR_EL2, x0
    adr     x0, el1_entry
    msr     ELR_EL2, x0
    ldr     x0, =0x80000000
    msr     HCR_EL2, x0
    isb
    eret

el1_entry:
    // Install the smallest identity map needed to execute this image.  The
    // common VMM replaces these tables once memory discovery is complete.
    ldr     x0, =boot_l0
    msr     TTBR0_EL1, x0
    // TTBR1 covers the canonical upper half. The bootstrap table is shared so
    // both halves translate the same low identity mappings until the common
    // VMM installs its final mappings.
    msr     TTBR1_EL1, x0
    // Attr0 is Device-nGnRE for MMIO and Attr1 is Normal WB for RAM.
    ldr     x0, =0x000000000000ff04
    msr     MAIR_EL1, x0
    // 48-bit VA, 48-bit PA, 4 KiB granule, inner-shareable WB memory.
    // T0SZ/T1SZ are both 16 and TG0/TG1 select 4 KiB granules.
    ldr     x0, =0x00000005b5103510
    msr     TCR_EL1, x0
    dsb     sy
    isb
    mrs     x0, SCTLR_EL1
    orr     x0, x0, #1
    orr     x0, x0, #4
    // Keep the early identity map uncached until the kernel installs final
    // page-table attributes.  This avoids relying on cache maintenance while
    // the loader and kernel image share the bootstrap mapping.
    orr     x0, x0, #4096
    msr     SCTLR_EL1, x0
    dsb     sy
    isb

    mov     x0, x19
    mov     x1, x20
    ldr     x2, ={entry}
    blr     x2
1:  wfe
    b       1b

// Emit a short diagnostic and stop if firmware enters an unsupported EL.
unsupported_el:
    mov     x0, #69
    bl      early_putc
    mov     x0, #76
    bl      early_putc
    mov     x0, #63
    bl      early_putc
2:  wfe
    b       2b

// PL011 early console on QEMU virt.  This is only used before Rust logging.
early_putc:
    ldr     x1, =0x09000000
    strb    w0, [x1]
    ret

.section .rodata
.balign 4096
// Level-0 table points at one level-1 table for the low 512 GiB.
boot_l0:
    .quad boot_l1 + 0x3
    .rept 511
    .quad 0
    .endr

.balign 4096
// One 1 GiB identity block covers QEMU's load address, GIC and early UART.
boot_l1:
    .quad 0x0000000000000701
    // The second 1 GiB block contains the kernel and RAM.  Select Attr1 so
    // exclusive atomics are permitted before the final kernel page tables.
    .quad 0x0000000040000705
    .rept 511
    .quad 0
    .endr
