use core::mem::MaybeUninit;

use explat::mem::BootMemoryRegions;
use expt::{
    PageTable, X86Level4PageTableMeta,
    pte::{MappingFlags, x86_64::X64PTE},
};
use memory_addr::VirtAddr;
use size_disp::SizeDisplay;

use crate::kprintln;

pub mod alloc;
mod early;
pub mod reloc;
pub mod sections;
pub mod vmm;

/// The physical address range occupied by the boot stack.
///
/// Stored during [`init_vmm`] (before VMM setup) so that the allocator
/// integration can exclude it from the buddy allocator.
static mut BOOT_STACK_RANGE: Option<memory_addr::PhysAddrRange> = None;

/// Saved boot memory regions.
///
/// Stored during [`init_vmm`] (before VMM setup) because the multiboot
/// info may become inaccessible after VMM setup (the `paddr_to_slice`
/// callback in the platform layer assumes identity mapping).
static mut SAVED_MEM_REGIONS: MaybeUninit<BootMemoryRegions> = MaybeUninit::uninit();

/// Saves the boot stack physical range for later retrieval.
///
/// Called once during [`init_vmm`] before VMM setup, so the allocator
/// integration can later exclude this range from the buddy allocator.
fn set_boot_stack_range(range: memory_addr::PhysAddrRange) {
    unsafe {
        core::ptr::write(core::ptr::addr_of_mut!(BOOT_STACK_RANGE), Some(range));
    }
}

/// Retrieves the boot stack physical range.
///
/// Panics if [`set_boot_stack_range`] has not been called yet.
pub fn boot_stack_range() -> memory_addr::PhysAddrRange {
    unsafe { (*core::ptr::addr_of!(BOOT_STACK_RANGE)).expect("boot stack range not set") }
}

/// Saves boot memory regions for later retrieval by [`saved_mem_regions`].
fn set_saved_mem_regions(regions: BootMemoryRegions) {
    unsafe {
        (*core::ptr::addr_of_mut!(SAVED_MEM_REGIONS)).write(regions);
    }
}

/// Retrieves the boot memory regions saved during [`init_vmm`].
///
/// Panics if [`set_saved_mem_regions`] has not been called yet.
pub fn saved_mem_regions() -> &'static BootMemoryRegions {
    unsafe { (*core::ptr::addr_of!(SAVED_MEM_REGIONS)).assume_init_ref() }
}

pub fn init_vmm(
    entry_with_vmm: *const exboot::KernelEntryType,
    hart_id: usize,
    arg: *const exboot::BootArg,
) -> ! {
    // Print kernel location before initializing the vmm.
    print_kernel_location();

    // Get physical memory regions from the boot argument.
    let mem_regions = explat::mem::boot_mem_regions(unsafe { arg.as_ref_unchecked().plat_arg })
        .expect("Memory info unavailable");
    print_mem_regions(&mem_regions);

    // Save memory regions for later use (after VMM setup the multiboot
    // info may become inaccessible because paddr_to_slice assumes
    // identity mapping).
    set_saved_mem_regions(mem_regions.clone());

    // Store the boot stack range before VMM setup modifies the BootArg data.
    {
        let bs = unsafe { arg.as_ref_unchecked() }.boot_stack;
        set_boot_stack_range(bs);
    }

    // Determine the layout of the virtual address space.
    vmm::init_vmm_layout();
    let virt_phys_offset = vmm::virt_phys_offset();

    // Initialize the early page allocator.
    let early_allocator_range = early::find_early_page_allocator_range(&mem_regions)
        .expect("No early page allocator range found");
    early::init_early_page_allocator(early_allocator_range.start);

    // Map the memory regions to the virtual address space.
    let mut early_page_table =
        PageTable::<X86Level4PageTableMeta, X64PTE>::new_alloc::<early::EarlyPagingHandler>()
            .unwrap();
    kprintln!("Early page table: {:x}\n", early_page_table.base_paddr());

    let flags: MappingFlags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
    for memory_region in &mem_regions {
        let paddr = memory_region.range.start;
        let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
        let vaddr_high = vaddr_low + virt_phys_offset;
        let size = memory_region.range.size();

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

    // Use a returnless call to jump to non-identical PC/SP.
    unsafe {
        let boot_stack_top = arg.as_ref_unchecked().boot_stack.end;
        let boot_stack_top = boot_stack_top.as_usize() + virt_phys_offset;
        let entry_with_vmm = entry_with_vmm.byte_add(virt_phys_offset);
        let arg = arg.byte_add(virt_phys_offset);

        call_fn_new_stack_arg2(entry_with_vmm as _, hart_id, arg as _, boot_stack_top)
    }
}

pub fn init_vmm_later() {
    print_kernel_location();

    let (bitmap, page_size_shift, base_paddr) = early::destroy_early_page_allocator();
    kprintln!(
        "Early page allocator destroyed: {:?}, {:x}, {:x}",
        bitmap,
        page_size_shift,
        base_paddr
    );

    // Store the early allocator range so that init_allocators can exclude it
    // from the buddy allocator.
    let early_range =
        memory_addr::PhysAddrRange::from_start_size(base_paddr, 512 << page_size_shift);
    early::set_early_allocator_range(early_range);
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

fn print_mem_regions(mem_regions: &BootMemoryRegions) {
    kprintln!("Physical memory regions:");
    for region in mem_regions {
        let start_usize = region.range.start.as_usize();
        let end_usize = region.range.end.as_usize();
        let size = region.range.size();
        let ty = region.type_;

        kprintln!(
            "  {:<#010x} - {:<#010x}, {}, {:?}",
            start_usize,
            end_usize,
            size.size_display_wide(),
            ty,
        );
    }

    kprintln!();
}

pub fn print_kernel_location() {
    kprintln!("Kernel location at: {:#x}", sections::kernel_range());
    for (name, range, aligned_range) in sections::all_sections() {
        kprintln!(
            "  {:<10}: {:#x}(..{:#x}), {} ({})",
            name,
            range,
            aligned_range.end,
            range.size().size_display_wide(),
            aligned_range.size().size_display_wide(),
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
