# fn switch_page_level(is_la57: bool);
.code64
.global switch_page_level
switch_page_level:
    # Fill the GDT base
    leaq    exarch_mem_gdt(%rip), %rax
    leaq    exarch_mem_gdt_desc(%rip), %rdx
    movl    %eax, 2(%rdx)

    # Load our GDT
    lgdtq   (%rdx)

    # Fill the blank in the page tables
    leaq    l4_pt_pdpt(%rip), %rax
    addq    $0x3, %rax
    leaq    l4_pt_pml4(%rip), %rdx
    movq    %rax, (%rdx)

    leaq    l5_pt_pml4(%rip), %rax
    addq    $0x3, %rax
    leaq    l5_pt_pml5(%rip), %rdx
    movq    %rax, (%rdx)

    # Save the is_la57 argument to %rbx
    movq    %rdi, %rbx

    # Load the page table addresses to %rcx here 
    # because IP-relative addressing doesn't work in 32-bit mode
    cmpq    $0, %rbx
    je      .Lload_pt_la48

.Lload_pt_la57:
    leaq    l5_pt_pml4(%rip), %rcx
    jmp     .Lload_pt_done

.Lload_pt_la48:
    leaq    l4_pt_pml4(%rip), %rcx

.Lload_pt_done:

    # Load 64-bit return address to %rdx
    leaq    switch_page_level64(%rip), %rdx

    # Exit long mode and jump to switch_page_level32
    pushq   $0x08
    leaq    switch_page_level(%rip), %rax
    pushq   %rax
    lretq

.code32
switch_page_level32:
    # Initialize data segments
    movw    $0x18, %ax
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
    cmpl    $0, %ebx
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
    pushl   $0x10
    pushl   %edx
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

    # Return to the caller
    retq

# The GDT
.section .rodata
.balign 8
exarch_mem_gdt_desc:
    .word   exarch_mem_gdt_end - exarch_mem_gdt - 1     # GDT limit
    # .long   exarch_mem_gdt                            # GDT base
    .long   0

.balign 16
exarch_mem_gdt:
    .quad 0x0000000000000000    # 0x00: null
    .quad 0x00cf9b000000ffff    # 0x08: code segment (base=0, limit=0xfffff, type=32bit code exec/read, DPL=0, 4k)
    .quad 0x00af9b000000ffff    # 0x10: code segment (base=0, limit=0xfffff, type=64bit code exec/read, DPL=0, 4k)
    .quad 0x00cf93000000ffff    # 0x18: data segment (base=0, limit=0xfffff, type=32bit data read/write, DPL=0, 4k)
exarch_mem_gdt_end:

# The identical L4 and L5 page tables
.section .rodata
.balign 4096
l4_pt_pml4:
    # 0x0000_0000 ~ 0x7f_ffff_ffff (512 GiB)
    # .quad l4_pt_pdpt + 0x3        # PRESENT | WRITABLE | paddr(boot_pt_pdpt)
    .fill 0, 8, 0
    .fill 511, 8, 0

.balign 4096
l4_pt_pdpt:
.set i, 0
.rept 512
    .quad 0x40000000 * i | 0x83     # PRESENT | WRITABLE | HUGE_PAGE | paddr(0x4000_0000 * i)
    .set i, i + 1
.endr

.balign 4096
l5_pt_pml5:
    # 0x0000_0000 ~ 0xffff_ffff_ffff (256 TiB)
    # .quad l5_pt_pml4 + 0x3        # PRESENT | WRITABLE | paddr(l5_pt_pdpt)
    .fill 0, 8, 0
    .fill 511, 8, 0

.balign 4096
l5_pt_pml4:
.set i, 0
.rept 512
    .quad 0x8000000000 * i | 0x83   # PRESENT | WRITABLE | HUGE_PAGE | paddr(0x80_0000_0000 * i)
    .set i, i + 1
.endr
