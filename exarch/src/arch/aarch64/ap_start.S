.section .text
.global _start_ap
.type _start_ap, %function

# The AP trampoline for AArch64 PSCI CPU_ON.
#
# When the control flow reaches here, x0 contains the physical address of the boot arguments
# structure (see `ApBootArgs` in power.rs).
#
# The trampoline must finish all architectural setup before branching to Rust.
# It does not return to firmware or to the caller.
_start_ap:
    # Mask interrupts.
    msr     daifset, #0xf

    # Read the AP boot arguments from the physical address in x0.
    # x4 = physical CPU ID
    # x5 = physical address of the lower translation-table root
    # x6 = physical address of the upper translation-table root
    # x7 = high-half virtual address of the AP stack top
    # x8 = high-half virtual address of the Rust AP entry
    ldp     x4, x5, [x0, #0]
    ldp     x6, x7, [x0, #16]
    ldr     x8, [x0, #32]

    # Install the independent roots in the two AArch64 translation regimes.
    msr     TTBR0_EL1, x5
    msr     TTBR1_EL1, x6

    # Match the bootstrap and kernel page-table attributes.
    # Attr0 is Device-nGnRE and Attr1 is inner-shareable normal write-back memory.
    # TODO: let the BSP pass it and TCR_EL1.
    ldr     x9, =0x000000000000ff04
    msr     MAIR_EL1, x9

    # Select 4 KiB granules and 48-bit lower and upper virtual-address spaces.
    ldr     x9, =0x00000005b5103510
    msr     TCR_EL1, x9
    dsb     sy
    isb     sy

    # Enable the MMU, data and instruction caches after the translation controls
    # are visible to the processor.
    mrs     x9, SCTLR_EL1
    orr     x9, x9, #1
    orr     x9, x9, #4
    orr     x9, x9, #4096
    msr     SCTLR_EL1, x9
    dsb     sy
    isb     sy

    # Set up the stack and pass the physical CPU ID to the Rust entry.
    mov     sp, x7
    mov     x0, x4

    # The Rust entry is a non-returning function. If it ever returns, stop the
    # AP instead of falling through into unrelated code.
    br      x8

1:  wfe
    b       1b

.size _start_ap, . - _start_ap
