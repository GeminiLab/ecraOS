# The start-up page for secondary CPUs.
#
# Technically, this file should be assembled separately with the static relocation model, and be
# then included into the ecraOS binary. However, we choose to include it here directly to avoid
# the complexity of the build system. As a result, we are forced to use some really nasty hacks
# when jumping across different code segments.

.section .rodata
.align 4096
.global ap_start_code_start
.code16
# This is expected to be placed at the start of the page {page_index}.
ap_start_code_start:
    # Disable the interrupts and invalidate the cache
    cli
    wbinvd

    # Clear the segment registers
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    # Load the GDT
    lgdt {gdt_desc}

    # Enable the protected mode
    movl %cr0, %eax
    orl $1, %eax
    movl %eax, %cr0

    # Load the address of the page
    movl ${page_index}, %edx
    shll $12, %edx

    # Setup the temporary stack at the top of the page
    movl %edx, %eax
    addl $0x1000, %eax
    movl %eax, %esp

    # Load the address of ap_start_code_entry32
    movl %edx, %eax
    addl $0x200, %eax

    # Jump to the protected mode
    pushl ${code32_selector}
    pushl %eax
    lretl

.align 512
.code32
# This is expected to be placed at {page_index} * 0x1000 + 0x200.
ap_start_code_entry32:
    # Setup the data segments
    movw ${data_selector}, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    # Write the CR4 register
    movl ${cr4}, %edx
    movl (%edx), %eax
    movl %eax, %cr4

    # Test cr4.la57 (bit 12), and write the CR3 register accordingly
    # movl ${page_table_root}, %edx
    # movl (%edx), %eax
    # movl %eax, %cr3

    andl $0x1000, %eax
    jz ap_wrcr3_pml4
    jmp ap_wrcr3_pml5

    ap_wrcr3_pml4:
    movl ${id_pt_pml4}, %edx
    movl (%edx), %eax
    movl %eax, %cr3
    jmp ap_wrcr3_done

    ap_wrcr3_pml5:
    movl ${id_pt_pml5}, %edx
    movl (%edx), %eax
    movl %eax, %cr3

    ap_wrcr3_done:

    # Write the EFER MSR
    movl ${efer}, %edx
    movl (%edx), %eax
    addl $0x4, %edx
    movl (%edx), %edx
    movl ${efer_msr}, %ecx
    wrmsr

    # Write the CR0 register
    movl ${cr0}, %edx
    movl (%edx), %eax
    movl %eax, %cr0

    # Load the address of the page
    movl ${page_index}, %edx
    shll $12, %edx

    # Load the address of ap_start_code_entry64
    movl %edx, %eax
    addl $0x400, %eax

    # Jump to the protected mode
    pushl ${code64_selector}
    pushl %eax
    lretl

.align 512
.code64
# This is expected to be placed at {page_index} * 0x1000 + 0x400.
ap_start_code_entry64:
    # Setup the stack from the absolute argument slot.
    movabsq ${stack_top}, %rdx
    movq (%rdx), %rsp

    # Switch from the temporary low PML4 to the real kernel root.
    movabsq ${page_table_root}, %rcx
    movq (%rcx), %rax
    movq %rax, %cr3

    # Load the physical CPU ID from the absolute argument slot.
    movabsq ${phys_cpu_id}, %rdx
    movq (%rdx), %rdi

    # Jump to the kernel AP entry from the absolute argument slot.
    movabsq ${entry_addr}, %rdx
    movq (%rdx), %rdx
    callq *%rdx

    2:
    hlt
    jmp 2b

.global ap_start_code_end
ap_start_code_end: