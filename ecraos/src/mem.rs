//! Kernel memory management.
//!
//! Provides virtual memory management (VMM), physical memory management (PMM),
//! early page allocation, and the final buddy+slab allocator integration.

use exarch::mem::{MemoryRegion, MemoryRegionFlags};
use expt::pte::MappingFlags;
use log::info;
use memory_addr::{PhysAddr, VirtAddr};
use size_disp::SizeDisplay;

use crate::kprintln;

pub mod allocs;
mod early;
pub mod pmm;
pub mod reloc;
pub mod sections;
pub mod vmm;

pub use early::BSP_STACK_SIZE;

pub fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
    try_virt_to_phys(addr).unwrap_or_else(|| PhysAddr::from_usize(addr.as_usize()))
}

/// Converts a physical address into its currently accessible virtual address.
pub fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
    vmm::direct_mapping_phys_to_virt(addr).unwrap_or_else(|| VirtAddr::from_usize(addr.as_usize()))
}

fn try_virt_to_phys(addr: VirtAddr) -> Option<PhysAddr> {
    let low_identity = PhysAddr::from_usize(addr.as_usize());
    if phys_addr_is_known(low_identity) {
        return Some(low_identity);
    }

    if let Some(paddr) = vmm::direct_mapping_virt_to_phys(addr)
        && phys_addr_is_known(paddr)
    {
        return Some(paddr);
    }

    if let Some(stack) = early::try_bsp_stack()
        && stack.alloc_range.contains(addr)
    {
        let offset = addr.as_usize() - stack.alloc_range.start.as_usize();
        return Some(stack.pa_range.start + offset);
    }

    allocs::vmalloc::virt_to_phys(addr)
}

fn phys_addr_is_known(addr: PhysAddr) -> bool {
    let Some(regions) = pmm::try_phys_mem_regions() else {
        return true;
    };

    regions.iter().any(|region| region.range.contains(addr))
}

pub fn init_and_enable_vmm(
    entry_with_vmm: *const ecraldr_base::KernelEntryType,
    hart_id: usize,
    arg: *const ecraldr_base::BootArg,
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
        unsafe { arg.as_ref_unchecked() }.plat_arg,
    );
    print_mem_regions("Final physical memory regions:", pmm::phys_mem_regions());
    // The final table is now the sole source of truth. boot_regions is never
    // used again.

    // Determine the layout of the virtual address space.
    vmm::init_vmm_layout();

    // Initialize the early page allocator using the final region table. It depends on the page
    // size info.
    early::init_early_page_allocator();

    // Create the page table and early mappings. It depends on the early page allocator.
    vmm::init_vmm_mapping_early::<early::EarlyPageAllocatorImpl>(pmm::phys_mem_regions());

    // Allocate the BSP stack area.
    early::init_bsp_stack(vmm::vmalloc_base());
    let bsp_stack = early::bsp_stack();

    vmm::with_page_table(|pt| {
        pt.map::<early::EarlyPageAllocatorImpl>(
            bsp_stack.alloc_range.start,
            bsp_stack.pa_range.start,
            bsp_stack.alloc_range.size(),
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .unwrap();
    });

    // Load the early page table.
    exarch::mem::set_page_table_root(vmm::page_table_root());

    // Call the relocation hook.
    exarch::reloc_hook::before_reloc();

    // Use a returnless call to jump to non-identical PC/SP.
    unsafe {
        let direct_mapping_offset = vmm::direct_mapping_offset();

        let new_stack_top = bsp_stack.alloc_range.end.as_usize();
        let entry_with_vmm = VirtAddr::from_usize(
            (entry_with_vmm as usize)
                .checked_add(direct_mapping_offset)
                .expect("relocated kernel entry address overflow"),
        );
        let arg = VirtAddr::from_usize(
            (arg as usize)
                .checked_add(direct_mapping_offset)
                .expect("relocated boot argument address overflow"),
        );

        call_fn_new_stack_arg2(
            entry_with_vmm.as_ptr_of::<fn(usize, usize) -> !>(),
            hart_id,
            arg.as_ptr_of::<ecraldr_base::BootArg>() as usize,
            new_stack_top,
        )
    }
}

pub fn init_after_enable_vmm() {
    print_kernel_location("Kernel location:");

    info!("Performing later memory initialization after enabling VMM...");

    // Initialize the page allocator.
    // TODO: recycle the loader memory region (as well as the bootstack).
    allocs::palloc::init_palloc();

    // Initialize the small object allocator.
    allocs::malloc::init_malloc_current_cpu();

    // Initialize the VMAllocator, and add the BSP stack/percpu area to it.
    let page_size_shift = vmm::page_size_shift();
    allocs::vmalloc::init_vmalloc(vmm::vmalloc_range(), page_size_shift);

    let early_bsp_stack = early::bsp_stack();
    allocs::vmalloc::register_external_range(
        early_bsp_stack.full_range,
        early_bsp_stack.alloc_range,
        early_bsp_stack.pa_range,
        MappingFlags::READ | MappingFlags::WRITE,
    )
    .expect("failed to add bsp stack to vmalloc");

    info!("Later memory initialization completed");
}

pub fn init_ap() {
    allocs::malloc::init_malloc_current_cpu();
}

pub fn remove_identical_mappings() {
    vmm::remove_identical_mapping::<vmm::TmpGoodPagingHandler>(pmm::phys_mem_regions());
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
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "mov rsp, {stack_top}",
            "call rax",
            stack_top = in(reg) stack_top,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rax") fn_ptr,
            options(preserves_flags, noreturn),
        );

        #[cfg(target_arch = "riscv64")]
        core::arch::asm!(
            "mv sp, {stack_top}",
            "jr a2",
            stack_top = in(reg) stack_top,
            in("a0") arg1,
            in("a1") arg2,
            in("a2") fn_ptr,
            options(preserves_flags, noreturn),
        );
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
