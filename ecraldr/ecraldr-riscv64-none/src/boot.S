.section .text.boot
.global _start
.type _start, @function
_start:
    # Save hartid and DTB pointer
    mv      s0, a0
    mv      s1, a1

    # Set stack pointer
    la      sp, {boot_stack_top}

    # Set up SV39 boot page table
    la      t0, boot_pt_sv39
    srli    t0, t0, 12  # PPN to bits 0-43
    li      t1, 8       # Sv39 mode
    slli    t1, t1, 60  # Mode to bits 60-63
    or      t0, t0, t1
    csrw    satp, t0
    sfence.vma

    # Call kernel entry
    mv      a0, s0
    mv      a1, s1
    la      a2, {entry}
    jalr    a2                      // rust_entry64_bsp(cpu_id, dtb)
    j       .

# Boot Page Table Entry Macro
.macro boot_pt_sv39_entry index
    # \index GiB ~ (\index + 1) GiB, VRWX_GAD, 1G block
    .quad (\index << 28) | 0xef
.endm

.section .rodata
.balign 4096
boot_pt_sv39:
# Low 256 GiB: Direct Mapping
.set i, 0
.rept 256
    boot_pt_sv39_entry i
    .set i, i + 1
.endr
# High 256 GiB: Not Mapped
.rept 256
    .quad 0
.endr
# End of Boot Page Table
.size boot_pt_sv39, . - boot_pt_sv39
