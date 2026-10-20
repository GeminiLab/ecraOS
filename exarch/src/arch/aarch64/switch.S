.section .text
.global exarch_task_switch
.type exarch_task_switch, %function
exarch_task_switch:
    stp x19, x20, [x0, #8]
    stp x21, x22, [x0, #24]
    stp x23, x24, [x0, #40]
    stp x25, x26, [x0, #56]
    stp x27, x28, [x0, #72]
    stp x29, x30, [x0, #88]
    mov x2, sp
    str x2, [x0]
    ldr x2, [x1]
    mov sp, x2
    ldp x19, x20, [x1, #8]
    ldp x21, x22, [x1, #24]
    ldp x23, x24, [x1, #40]
    ldp x25, x26, [x1, #56]
    ldp x27, x28, [x1, #72]
    ldp x29, x30, [x1, #88]
    ret
.size exarch_task_switch, . - exarch_task_switch

.global context_entry_trampoline
.type context_entry_trampoline, %function
context_entry_trampoline:
    mov x0, x19
    br x20
.size context_entry_trampoline, . - context_entry_trampoline
