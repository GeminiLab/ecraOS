//! Kernel memory management.
//!
//! Provides virtual memory management (VMM), physical memory management (PMM),
//! early page allocation, and the final buddy+slab allocator integration.

use exarch::mem::{MemoryRegion, MemoryRegionFlags};
use expt::pte::MappingFlags;
use memory_addr::VirtAddrRange;
use size_disp::SizeDisplay;

use crate::kprintln;

pub mod alloc;
mod early;
pub mod pmm;
pub mod reloc;
pub mod sections;
pub mod vmalloc;
pub mod vmm;

const TEMP_KERNEL_STACK_SIZE: usize = 16384;

pub fn init_and_enable_vmm(
    entry_with_vmm: *const exboot::KernelEntryType,
    hart_id: usize,
    arg: *const exboot::BootArg,
) -> ! {
    // Print kernel location before initializing the vmm.
    print_kernel_location("Kernel location immediately after boot:");

    // Get physical memory regions from the boot argument.
    let raw_mem_regions = exarch::mem::raw_mem_regions(unsafe { arg.as_ref_unchecked().plat_arg });
    print_mem_regions("Raw memory regions reported by platform:", &raw_mem_regions);

    // Build the final physical memory region table by splitting raw boot
    // regions around the loader and kernel image.
    pmm::build_phys_mem_regions(
        &raw_mem_regions,
        unsafe { arg.as_ref_unchecked() }.loader_range,
    );
    print_mem_regions("Final physical memory regions:", pmm::phys_mem_regions());
    // The final table is now the sole source of truth. boot_regions is never
    // used again.

    // Determine the layout of the virtual address space.
    vmm::init_vmm_layout();
    let virt_phys_offset = vmm::virt_phys_offset();
    let page_size_shift = vmm::page_size_shift();

    // Initialize the early page allocator using the final region table. It depends on the page
    // size info.
    early::init_early_page_allocator();

    // Create the page table and early mappings. It depends on the early page allocator.
    vmm::init_vmm_mapping_early::<early::EarlyPagingHandler>(pmm::phys_mem_regions());

    // Allocate new stack for the BSP.
    let kernel_stack_pages = TEMP_KERNEL_STACK_SIZE.div_ceil(1 << page_size_shift);

    let kernel_stack_region_start = vmm::vmalloc_base();
    let kernel_stack_start = kernel_stack_region_start + (1usize << page_size_shift);
    let kernel_stack_end = kernel_stack_start + (kernel_stack_pages << page_size_shift);
    let kernel_stack_region_end = kernel_stack_end + (1usize << page_size_shift);

    unsafe {
        vmm::TEMP_BSP_KERNEL_STACK =
            VirtAddrRange::new_unchecked(kernel_stack_region_start, kernel_stack_region_end);
    }

    let kernel_stack_paddr = early::alloc_page_aligned(kernel_stack_pages << page_size_shift)
        .expect("failed to allocate kernel stack");

    vmm::with_page_table(|pt| {
        pt.map::<early::EarlyPagingHandler>(
            kernel_stack_start,
            kernel_stack_paddr,
            kernel_stack_pages << page_size_shift,
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .unwrap()
    });

    // Load the early page table.
    exarch::mem::set_page_table_root(vmm::with_page_table(|pt| pt.root_paddr()));

    // Use a returnless call to jump to non-identical PC/SP.
    unsafe {
        let new_stack_top = kernel_stack_end.as_usize();
        let entry_with_vmm = entry_with_vmm.byte_add(virt_phys_offset);
        let arg = arg.byte_add(virt_phys_offset);

        call_fn_new_stack_arg2(entry_with_vmm as _, hart_id, arg as _, new_stack_top)
    }
}

pub fn init_after_enable_vmm() {
    print_kernel_location("Kernel location after VMM setup:");

    // TODO: recycle the loader memory region (as well as the bootstack).
    alloc::init_allocators();

    // Remove identical mappings. We cannot do this before the allocator is initialized, because
    // this will destroy the early allocator effectively.
    vmm::init_vmm_mapping_after::<vmm::TmpGoodPagingHandler>(pmm::phys_mem_regions());
}

/// Converts [`MemoryRegionFlags`] to [`MappingFlags`] for page table entries.
fn region_flags_to_mapping(flags: MemoryRegionFlags) -> MappingFlags {
    let mut mapping = MappingFlags::empty();
    if flags.contains(MemoryRegionFlags::READ) {
        mapping |= MappingFlags::READ;
    }
    if flags.contains(MemoryRegionFlags::WRITE) {
        mapping |= MappingFlags::WRITE;
    }
    if flags.contains(MemoryRegionFlags::EXECUTE) {
        mapping |= MappingFlags::EXECUTE;
    }
    if flags.contains(MemoryRegionFlags::DEVICE) {
        mapping |= MappingFlags::DEVICE;
    }
    if flags.contains(MemoryRegionFlags::UNCACHED) {
        mapping |= MappingFlags::UNCACHED;
    }
    mapping
}

unsafe fn call_fn_new_stack_arg2(
    fn_ptr: *const fn(usize, usize) -> !,
    arg1: usize,
    arg2: usize,
    stack_top: usize,
) -> ! {
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack_top}",
            "call rax",
            stack_top = in(reg) stack_top,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rax") fn_ptr,
            options(preserves_flags, noreturn),
        )
    }
}

fn print_mem_regions(heading: &str, mem_regions: &[MemoryRegion]) {
    kprintln!("{}", heading);
    for region in mem_regions {
        let start_usize = region.range.start.as_usize();
        let end_usize = region.range.end.as_usize();
        let size = region.range.size();

        kprintln!(
            "  {:<#018x} - {:<#018x}, {}, {} ({})",
            start_usize,
            end_usize,
            size.size_display_wide(),
            region.flags,
            region.desc,
        );
    }

    kprintln!();
}

pub fn print_kernel_location(heading: &str) {
    kprintln!("{}", heading);
    kprintln!("  Kernel range: {:#x}", sections::kernel_range());
    for section in sections::all_sections() {
        kprintln!(
            "    {:<10}: {:#x}(aligned {:#x}), {} (aligned {})",
            section.name,
            section.range,
            section.aligned_range,
            section.range.size().size_display_wide(),
            section.aligned_range.size().size_display_wide(),
        );
    }

    kprintln!();
}

pub fn clear_bss() {
    let bss_range = sections::bss();
    let bss_slice =
        unsafe { core::slice::from_raw_parts_mut(bss_range.start.as_mut_ptr(), bss_range.size()) };

    bss_slice.fill(0);
}
