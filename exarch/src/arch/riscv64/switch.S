.section .text
.align 2
.global exarch_task_switch
.type exarch_task_switch, @function
exarch_task_switch:
    sd ra,  0x00(a0)
    sd sp,  0x08(a0)
    sd s0,  0x10(a0)
    sd s1,  0x18(a0)
    sd s2,  0x20(a0)
    sd s3,  0x28(a0)
    sd s4,  0x30(a0)
    sd s5,  0x38(a0)
    sd s6,  0x40(a0)
    sd s7,  0x48(a0)
    sd s8,  0x50(a0)
    sd s9,  0x58(a0)
    sd s10, 0x60(a0)
    sd s11, 0x68(a0)
    ld ra,  0x00(a1)
    ld sp,  0x08(a1)
    ld s0,  0x10(a1)
    ld s1,  0x18(a1)
    ld s2,  0x20(a1)
    ld s3,  0x28(a1)
    ld s4,  0x30(a1)
    ld s5,  0x38(a1)
    ld s6,  0x40(a1)
    ld s7,  0x48(a1)
    ld s8,  0x50(a1)
    ld s9,  0x58(a1)
    ld s10, 0x60(a1)
    ld s11, 0x68(a1)
    ret
.size exarch_task_switch, . - exarch_task_switch

.global context_entry_trampoline
.type context_entry_trampoline, @function
context_entry_trampoline:
    ld a0, 0(sp)
    ld t0, 8(sp)
    addi sp, sp, 16
    jalr t0
    unimp
.size context_entry_trampoline, . - context_entry_trampoline
