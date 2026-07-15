# fn switch_page_level(is_la57: bool);
.code64
.global switch_page_level
switch_page_level:
    # Save callee-saved register %rbx
    pushq   %rbx

    # Fill the blank in the page tables
    leaq    exarch_pt_pdpt(%rip), %rax
    addq    $0x3, %rax
    leaq    exarch_pt_pml4(%rip), %rdx
    movq    %rax, (%rdx)

    leaq    exarch_pt_pml4(%rip), %rax
    addq    $0x3, %rax
    leaq    exarch_pt_pml5(%rip), %rdx
    movq    %rax, (%rdx)

    # Load the page table addresses to %rcx here 
    # because IP-relative addressing doesn't work in 32-bit mode
    cmpq    $0, %rdi
    je      .Lload_pt_la48

.Lload_pt_la57:
    leaq    exarch_pt_pml5(%rip), %rcx
    jmp     .Lload_pt_done

.Lload_pt_la48:
    leaq    exarch_pt_pml4(%rip), %rcx

.Lload_pt_done:

    # Load 64-bit return address to %rsi
    leaq    switch_page_level64(%rip), %rsi

    # Exit long mode and jump to switch_page_level32
    pushq   ${code32_selector}
    leaq    switch_page_level32(%rip), %rax
    pushq   %rax
    lretq

.code32
switch_page_level32:
    # Initialize data segments
    movw    ${data_selector}, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %fs
    movw    %ax, %gs
    movw    %ax, %ss

    # Disable paging
    movl    %cr0, %eax
    andl    $~0x80000000, %eax
    movl    %eax, %cr0

    # Update CR4.LA57
    movl    %cr4, %eax
    cmpl    $0, %edi
    je      .Lla48

.Lla57:
    orl     $0x00001000, %eax
    jmp     .Lwrite_cr4

.Lla48:
    andl    $~0x00001000, %eax

.Lwrite_cr4:
    movl    %eax, %cr4

    # Write CR3
    movl    %ecx, %cr3

    # Re-enable paging
    movl    %cr0, %eax
    orl     $0x80000000, %eax
    movl    %eax, %cr0

    # Re-enable long mode
    movl    $0xC0000080, %ecx
    rdmsr
    orl     $0x00000100, %eax
    wrmsr

    # Return to 64-bit mode
    pushl   ${code64_selector}
    pushl   %esi
    lretl

.code64
switch_page_level64:
    # Clear data segments
    xorq    %rax, %rax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %fs
    movw    %ax, %gs
    movw    %ax, %ss

    # Restore callee-saved register %rbx
    popq    %rbx

    # Return to the caller
    retq

# The identical page tables
.section .data
.balign 4096
.global exarch_pt_pml5
exarch_pt_pml5:
    # 0x0000_0000 ~ 0xffff_ffff_ffff (256 TiB)
    # .quad exarch_pt_pml4 + 0x3    # PRESENT | WRITABLE | paddr(exarch_pt_pml4)
    .fill 0, 8, 0
    .fill 511, 8, 0

.balign 4096
.global exarch_pt_pml4
exarch_pt_pml4:
    # 0x0000_0000 ~ 0x7f_ffff_ffff (512 GiB)
    # .quad exarch_pt_pdpt + 0x3    # PRESENT | WRITABLE | paddr(exarch_pt_pdpt)
    .fill 0, 8, 0
    .fill 511, 8, 0

.balign 4096
exarch_pt_pdpt:
.set i, 0
.rept 512
    .quad 0x40000000 * i | 0x83     # PRESENT | WRITABLE | HUGE_PAGE | paddr(0x4000_0000 * i)
    .set i, i + 1
.endr
