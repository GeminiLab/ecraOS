# The entry point for the APs
#
# It's required that when an AP starts:
# - `a0` contains the hart_id (guaranteed by the hardware)
# - `a1` contains the physical address of the stack top
# - `-8(a1)` (i.e. the stack top) contains the expected satp value
# - `-16(a1)` contains the virtual address of the stack top
# - `-24(a1)` contains the virtual address of the APEntry
.section .text
.global _start_ap
_start_ap:
    ld t0, -8(a1)
    ld t1, -16(a1)
    ld t2, -24(a1)
    
    # Load the page table root
    csrw satp, t0
    sfence.vma

    # Load the stack top
    mv sp, t1

    # Clear a1 anyway
    li a1, 0

    # Jump to the APEntry
    jalr x0, 0(t2)
