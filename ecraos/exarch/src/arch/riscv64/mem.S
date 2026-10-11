# Exarch Page Table Entry Macro
.macro exarch_pt_sv39_entry index
    # \index GiB ~ (\index + 1) GiB, VRWX_GAD, 1G block
    .quad (\index << 28) | 0xef
.endm

.macro exarch_pt_sv48_entry index
    # \index * 512 GiB ~ (\index + 1) * 512 GiB, VRWX_GAD, 512 GiB block
    .quad (\index << 37) | 0xef
.endm

.macro exarch_pt_sv57_entry index
    # \index * 256 TiB ~ (\index + 1) * 256 TiB, VRWX_GAD, 256 TiB block
    .quad (\index << 46) | 0xef
.endm

.section .rodata
.balign 4096
exarch_pt_sv39:
# Low 256 GiB: Direct Mapping
.set i, 0
.rept 256
    exarch_pt_sv39_entry i
    .set i, i + 1
.endr
# High 256 GiB: Not Mapped
.rept 256
    .quad 0
.endr
.size exarch_pt_sv39, . - exarch_pt_sv39

.balign 4096
exarch_pt_sv48:
# Low 128 TiB: Direct Mapping
.set i, 0
.rept 256
    exarch_pt_sv48_entry i
    .set i, i + 1
.endr
# High 128 TiB: Not Mapped
.rept 256
    .quad 0
.endr

.balign 4096
exarch_pt_sv57:
# First 32 PiB: Direct Mapping
.set i, 0
.rept 128
    exarch_pt_sv57_entry i
    .set i, i + 1
.endr
# Other 96 PiB: Not Mapped
.rept 384
    .quad 0
.endr
.size exarch_pt_sv57, . - exarch_pt_sv57

.macro write_satp root, mode
    lla     a0, \root
    srli    a0, a0, 12
    li      a1, \mode
    slli    a1, a1, 60
    or      a0, a0, a1
    csrw    satp, a0
    sfence.vma
.endm

.macro read_satp_mode
    csrr    a0, satp
    srli    a0, a0, 60
.endm

.section .text
# extern "C" fn detect_max_va_bits() -> usize 
.global detect_max_va_bits
detect_max_va_bits:
    # Called from Rust: keep callee-saved registers intact.
    csrr    t0, satp

    # Test sv57
    write_satp  exarch_pt_sv57, 10
    read_satp_mode
    li      a1, 10
    beq     a0, a1, .Ldone

    # Test sv48
    write_satp  exarch_pt_sv48, 9
    read_satp_mode
    li      a1, 9
    beq     a0, a1, .Ldone

    # Test sv39
    write_satp  exarch_pt_sv39, 8
    read_satp_mode
    li      a1, 8
    beq     a0, a1, .Ldone

    # Something strange happened!
    li      a0, 0
    j       .Ldone

.Ldone:
    csrw    satp, t0
    sfence.vma
    j       mode_to_va_bits

# extern "C" fn detect_current_va_bits() -> usize 
.global detect_current_va_bits
detect_current_va_bits:
    read_satp_mode
    j       mode_to_va_bits

# extern "C" fn mode_to_va_bits(mode: usize) -> usize
.global mode_to_va_bits
mode_to_va_bits:
    li      a1, 10
    beq     a0, a1, .Lsv57_found
    li      a1, 9
    beq     a0, a1, .Lsv48_found
    li      a1, 8
    beq     a0, a1, .Lsv39_found
    li      a0, 0
    ret

.Lsv57_found:
    li      a0, 57
    ret

.Lsv48_found:
    li      a0, 48
    ret

.Lsv39_found:
    li      a0, 39
    ret

# extern "C" fn set_va_bits(mode: usize) -> bool
.global set_va_bits
set_va_bits:
    li      a1, 57
    beq     a0, a1, .Lset_sv57
    li      a1, 48
    beq     a0, a1, .Lset_sv48
    li      a1, 39
    beq     a0, a1, .Lset_sv39
    li      a0, 0
    ret

.Lset_sv57:
    write_satp  exarch_pt_sv57, 10
    li      a0, 1
    ret

.Lset_sv48:
    write_satp  exarch_pt_sv48, 9
    li      a0, 1
    ret

.Lset_sv39:
    write_satp  exarch_pt_sv39, 8
    li      a0, 1
    ret
