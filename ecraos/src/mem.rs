use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange, va};

use explat::init::{EarlyMemoryInfo, VAHalfStatus};
use expt::{
    PageTable, X86Level4PageTableMeta,
    pte::{MappingFlags, x86_64::X64PTE},
};
use size_disp::SizeDisplay;

use crate::early_println;

mod early;
pub mod reloc;
pub mod sections;

use early::EARLY_PAGE_ALLOCATOR_SIZE;

pub fn init_vmm(memory_info: EarlyMemoryInfo, boot_stack: PhysAddrRange) {
    let identical_kernel_range = sections::kernel_range();

    // Print kernel location and early memory info before initializing the vmm.
    print_kernel_location();
    print_early_mem_info(&memory_info);

    // find
    let early_allocator_range =
        find_early_page_allocator_range(&memory_info).expect("No early page allocator range found");
    early_println!(
        "Early page allocator range: {:x}, {}\n",
        early_allocator_range,
        early_allocator_range.size().size_display_wide()
    );
    early::init_early_page_allocator(early_allocator_range.start);

    let upper_bits = get_va_upper_bits(&memory_info);
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    early_println!("Virtual address space:");
    early_println!("  Upper half start    : {:#x}", upper_start);

    // Top level memory areas:
    // - 1st half: direct mapping area
    // - 2nd half:
    //   - 3rd quater: vmalloc area
    //   - 4th quater: not used
    // This scheme gives us at least the same size of these areas as Linux does.
    let quater_size = 1usize << (upper_bits - 2);
    let direct_mapping_range = VirtAddrRange::from_start_size(upper_start, quater_size * 2);
    let vmalloc_range = VirtAddrRange::from_start_size(upper_start + quater_size * 2, quater_size);
    early_println!(
        "  Direct mapping area : {:x}, {}",
        direct_mapping_range,
        direct_mapping_range.size().size_display_wide()
    );
    early_println!(
        "  Vmalloc area        : {:x}, {}",
        vmalloc_range,
        vmalloc_range.size().size_display_wide()
    );

    let offset = direct_mapping_range.start;

    let mut early_page_table =
        PageTable::<X86Level4PageTableMeta, X64PTE>::new_alloc::<early::EarlyPagingHandler>()
            .unwrap();
    early_println!("Early page table: {:x}", early_page_table.base_paddr());

    let flags: MappingFlags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
    for memory_region in &memory_info.memory_regions {
        let paddr = memory_region.start.into();
        let vaddr_low = memory_region.start.into();
        let vaddr_high = offset + memory_region.start;
        let size = memory_region.size;

        early_page_table
            .map::<early::EarlyPagingHandler>(vaddr_low, paddr, size, flags)
            .unwrap();
        early_page_table
            .map::<early::EarlyPagingHandler>(vaddr_high, paddr, size, flags)
            .unwrap();
    }

    unsafe {
        core::arch::asm!(
            "mov cr3, rax",
            in("rax") early_page_table.base_paddr().as_usize()
        );
    }

    // Move RIP and RSP into the direct-mapped high range. The boot stack stays
    // at the same physical addresses; only the virtual addresses change.
    unsafe {
        let off = offset.as_usize();
        core::arch::asm!(
            "lea rax, [rsp + {off}]",
            "mov rsp, rax",
            "lea rax, [rip + 2f]",
            "add rax, {off}",
            "jmp rax",
            "2:",
            off = in(reg) off,
            out("rax") _,
        )
    }

    unsafe {
        reloc::relocate_me();
    }

    print_kernel_location();

    // We are in the direct mapping area now. However, there are still pointers
    // to the identity map in the stack. We need to fix them. It's still ok to
    // use the old pointers here because the identity map is still valid.
    const USIZE_WIDTH: usize = size_of::<usize>();
    let mut boot_stack_ptr = VirtAddr::from(boot_stack.start.as_usize()).align_up(USIZE_WIDTH);
    let boot_stack_top = VirtAddr::from(boot_stack.end.as_usize()).align_down(USIZE_WIDTH);

    early_println!(
        "Fixing boot stack pointers... [{:#x}, {:#x})",
        boot_stack_ptr,
        boot_stack_top
    );

    while boot_stack_ptr < boot_stack_top {
        let ptr = boot_stack_ptr.as_mut_ptr_of::<usize>();

        unsafe {
            if identical_kernel_range.contains((*ptr).into()) {
                *ptr += offset.as_usize();
            }
        }

        boot_stack_ptr += USIZE_WIDTH;
    }

    early_println!("Boot stack pointers fixed");

    // Page-table walks must use the direct map; identity map is about to go away.
    early::NotVeryEarlyPagingHandler::set_offset(offset);
    for memory_region in &memory_info.memory_regions {
        let vaddr_low = memory_region.start.into();
        let size = memory_region.size;

        early_page_table
            .unmap::<early::NotVeryEarlyPagingHandler>(vaddr_low, size)
            .unwrap();
    }

    // flush TLBs
    unsafe {
        core::arch::asm!(
            "mov {tmp}, cr3",
            "mov cr3, {tmp}",
            tmp = out(reg) _,
            options(nostack),
        );
    }

    let (a, b) = early::destroy_early_page_allocator();
    early_println!("Early page allocator destroyed: {:?}, {:x}", a, b);
}

fn find_early_page_allocator_range(memory_info: &EarlyMemoryInfo) -> Option<PhysAddrRange> {
    let kernel_range = sections::kernel_range();
    let kernel_range = PhysAddrRange::new(
        kernel_range.start.as_usize().into(),
        kernel_range.end.as_usize().into(),
    );

    for region in (&memory_info.memory_regions).into_iter().rev() {
        let start_aligned = PhysAddr::from(region.start).align_up(EARLY_PAGE_ALLOCATOR_SIZE);
        let end_aligned =
            PhysAddr::from(region.start + region.size).align_down(EARLY_PAGE_ALLOCATOR_SIZE);
        let mut range = PhysAddrRange::from_start_size(
            end_aligned - EARLY_PAGE_ALLOCATOR_SIZE,
            EARLY_PAGE_ALLOCATOR_SIZE,
        );

        while range.start >= start_aligned {
            if !range.overlaps(kernel_range) {
                return Some(range);
            }

            range.start -= EARLY_PAGE_ALLOCATOR_SIZE;
            range.end -= EARLY_PAGE_ALLOCATOR_SIZE;
        }
    }

    None
}

fn print_early_mem_info(memory_info: &EarlyMemoryInfo) {
    explat::dbcn_println!("Early memory info:");
    explat::dbcn_println!("  Physical memory regions:");
    for region in &memory_info.memory_regions {
        explat::dbcn_println!(
            "    {:<#010x} - {:<#010x}, {}",
            region.start,
            region.start + region.size,
            region.size.size_display_wide(),
        );
    }

    explat::dbcn_println!("  Virtual address space:");
    fn print_half_support(which: &str, support: VAHalfStatus) {
        match support {
            VAHalfStatus::NotSupported => explat::dbcn_println!("    {:<10}: Not supported", which),
            VAHalfStatus::Disabled { max_bits } => explat::dbcn_println!(
                "    {:<10}: Supported but disabled, max {max_bits} bits",
                which
            ),
            VAHalfStatus::Enabled {
                current_bits,
                max_bits,
            } => explat::dbcn_println!(
                "    {:<10}: Supported and enabled with {current_bits} bits, max {max_bits} bits",
                which
            ),
        }
    }

    print_half_support("Lower half", memory_info.va_lower_half_status);
    print_half_support("Upper half", memory_info.va_upper_half_status);

    explat::dbcn_println!();
}

fn get_va_upper_bits(memory_info: &EarlyMemoryInfo) -> u32 {
    if sections::kernel_range().start.as_usize() & (1 << (usize::BITS - 1)) != 0 {
        unimplemented!(
            "Booting directly in the upper half of the virtual address space is not supported yet"
        );
    }

    match memory_info.va_upper_half_status {
        VAHalfStatus::NotSupported => unimplemented!(
            "Upper half of the virtual address space is not supported, running in the lower half is not supported yet"
        ),
        VAHalfStatus::Disabled { .. } => unimplemented!(
            "Upper half of the virtual address space is disabled, VA adjustment is not supported yet"
        ),
        VAHalfStatus::Enabled {
            current_bits,
            max_bits,
        } => {
            if current_bits != max_bits {
                explat::dbcn_println!(
                    "Upper half of the virtual address space is not fully enabled, VA adjustment is not supported yet"
                );
                explat::dbcn_println!("Using the current VA bits ({current_bits})");
            }

            current_bits
        }
    }
}

pub fn print_kernel_location() {
    early_println!("Kernel location at: {:#x}", sections::kernel_range());
    for (name, range) in sections::all_sections() {
        early_println!(
            "  {:<10}: {:#x} ({})",
            name,
            range,
            range.size().size_display_wide(),
        );
    }

    early_println!();
}

pub fn clear_bss() {
    let bss_range = sections::bss();
    let bss_slice =
        unsafe { core::slice::from_raw_parts_mut(bss_range.start.as_mut_ptr(), bss_range.size()) };

    bss_slice.fill(0);
}
