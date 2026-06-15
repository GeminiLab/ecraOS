//! Kernel memory management.
//!
//! Provides virtual memory management (VMM), physical memory management (PMM),
//! early page allocation, and the final buddy+slab allocator integration.

use exarch::mem::{MemoryRegion, MemoryRegionFlags};
use expt::pte::MappingFlags;
use log::info;
use size_disp::SizeDisplay;

use crate::kprintln;

pub mod alloc;
mod early;
pub mod percpu;
pub mod pmm;
pub mod reloc;
pub mod sections;
pub mod vmalloc;
pub mod vmm;

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

    // Initialize the early page allocator using the final region table. It depends on the page
    // size info.
    early::init_early_page_allocator();

    // Create the page table and early mappings. It depends on the early page allocator.
    vmm::init_vmm_mapping_early::<early::EarlyPageAllocatorImpl>(pmm::phys_mem_regions());

    // Allocate the BSP stack and percpu area.
    early::init_bsp_stack(vmm::vmalloc_base());
    let (bsp_range_with_guards, bsp_range, bsp_pa_range) = early::bsp_stack();

    early::init_bsp_percpu(bsp_range_with_guards.end);
    let (_, percpu_range, percpu_pa_range) = early::bsp_percpu();

    vmm::with_page_table(|pt| {
        pt.map::<early::EarlyPageAllocatorImpl>(
            bsp_range.start,
            bsp_pa_range.start,
            bsp_range.size(),
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .unwrap();

        pt.map::<early::EarlyPageAllocatorImpl>(
            percpu_range.start,
            percpu_pa_range.start,
            percpu_range.size(),
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .unwrap();
    });

    // Load the early page table.
    exarch::mem::set_page_table_root(vmm::with_page_table(|pt| pt.root_paddr()));

    // Use a returnless call to jump to non-identical PC/SP.
    unsafe {
        let new_stack_top = bsp_range.end.as_usize();
        let entry_with_vmm = entry_with_vmm.byte_add(virt_phys_offset);
        let arg = arg.byte_add(virt_phys_offset);

        call_fn_new_stack_arg2(entry_with_vmm as _, hart_id, arg as _, new_stack_top)
    }
}

pub fn init_after_enable_vmm() {
    print_kernel_location("Kernel location:");

    info!("Performing later memory initialization after enabling VMM...");

    // TODO: recycle the loader memory region (as well as the bootstack).
    alloc::init_allocators();

    // Remove identical mappings. We cannot do this before the allocator is initialized, because
    // this will destroy the early allocator effectively.
    vmm::remove_identical_mapping::<vmm::TmpGoodPagingHandler>(pmm::phys_mem_regions());

    // Initialize the BSP per-CPU area.
    let (percpu_range, percpu_alloc_range, percpu_pa_range) = early::bsp_percpu();
    unsafe { expercpu::init(percpu_alloc_range.start) };

    // The initialization of the memory allocators (slab) should be moved here.
    // alloc::init_malloc();

    // Initialize the VMAllocator, and add the BSP stack/percpu area to it.
    let page_size_shift = vmm::page_size_shift();
    vmalloc::init_vmalloc(vmm::vmalloc_range(), page_size_shift);

    let (stack_range, alloc_range, pa_range) = early::bsp_stack();
    vmalloc::VMALLOC
        .lock()
        .add_allocated_range(stack_range, alloc_range, pa_range)
        .expect("failed to add bsp stack to vmalloc");

    vmalloc::VMALLOC
        .lock()
        .add_allocated_range(percpu_range, percpu_alloc_range, percpu_pa_range)
        .expect("failed to add bsp percpu area to vmalloc");

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
