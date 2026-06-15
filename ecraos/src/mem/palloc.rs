//! Kernel physical page allocator integration.

use exarch::mem::MemoryRegionFlags;
use exbuddy::{AllocatorStats, BuddyAllocator};
use kspin::SpinNoIrq;
use log::{debug, info};
use memory_addr::PhysAddr;
use size_disp::SizeDisplay;

use crate::{kprintln, mem};

static BUDDY: SpinNoIrq<BuddyAllocator> = SpinNoIrq::new(BuddyAllocator::new());

pub fn init_palloc() {
    let phys_regions = mem::pmm::phys_mem_regions();
    let page_size_shift = mem::vmm::page_size_shift();
    let page_size = mem::vmm::page_size();
    let vpo = mem::vmm::virt_phys_offset();

    info!("Initializing page allocator...");

    let early_alloc_range = mem::early::phys_addr_range();
    let (early_alloc_bitmap, _, early_allocbase_paddr) = mem::early::destroy_early_page_allocator();

    debug!(
        "Early page allocator at {:x} destroyed, {} pages allocated",
        early_alloc_range.start,
        early_alloc_bitmap.len()
    );

    let mut buddy = BUDDY.lock();
    // SAFETY: we believe that we have mapped the physical memory to the virtual
    // memory correctly.
    unsafe { buddy.init(page_size_shift, vpo) }.expect("failed to init buddy allocator");

    for region in phys_regions {
        if !region.flags.contains(MemoryRegionFlags::FREE) {
            continue;
        }

        if early_alloc_range.overlaps(region.range) {
            if buddy
                .check_metadata_overlap(region.range, early_alloc_range)
                .unwrap()
            {
                panic!(
                    "Buddy metadata overlaps early allocator for region {:x} ({})",
                    region.range, region.desc
                );
            }
        }

        debug!(
            "Adding region {:x} ({}) to buddy allocator",
            region.range, region.desc
        );
        // SAFETY: we trust exarch to provide a valid physical memory map for
        // the active architecture.
        unsafe { buddy.add_section(region.range) }
            .expect("failed to add the previous region to buddy allocator");
    }

    assert!(
        buddy.section_count() > 0,
        "no usable memory region found for buddy allocator"
    );

    debug!("Marking early allocator's in-use pages as allocated in the buddy");
    for index in &early_alloc_bitmap {
        buddy
            .alloc_blocks_at(early_allocbase_paddr + (index * page_size), 1)
            .expect("failed to mark early allocator page as in-use");
    }
    drop(buddy);

    info!(
        "Page allocator initialized, {} pages allocatable in total",
        stats().heap_pages()
    );
    print_buddy_stats("Initial buddy allocator stats");
}

pub fn alloc_frame() -> exbuddy::BuddyResult<PhysAddr> {
    BUDDY.lock().alloc_frame()
}

pub fn alloc_frames(count: usize, align: usize) -> exbuddy::BuddyResult<PhysAddr> {
    let page_size = mem::vmm::page_size();
    BUDDY.lock().alloc_frames(count, align.max(page_size))
}

pub fn alloc_frames_at(paddr: PhysAddr, count: usize) -> exbuddy::BuddyResult<PhysAddr> {
    BUDDY.lock().alloc_blocks_at(paddr, count)?;
    Ok(paddr)
}

pub fn dealloc_frames(addr: PhysAddr, count: usize) -> exbuddy::BuddyResult {
    BUDDY.lock().dealloc_frames(addr, count)
}

pub fn dealloc_frame(addr: PhysAddr) -> exbuddy::BuddyResult {
    BUDDY.lock().dealloc_frame(addr)
}

pub fn dealloc_blocks_at(addr: PhysAddr, count: usize) -> exbuddy::BuddyResult {
    BUDDY.lock().dealloc_blocks_at(addr, count)
}

pub fn is_allocated(addr: PhysAddr) -> exbuddy::BuddyResult<bool> {
    BUDDY.lock().is_allocated(addr)
}

pub fn stats() -> AllocatorStats {
    BUDDY.lock().stats()
}

pub fn print_buddy_stats(heading: &str) {
    let stats = stats();
    let page_size_shift = mem::vmm::page_size_shift();
    let total = stats.total_pages();
    let meta = stats.meta_pages();
    let heap = stats.heap_pages();
    let used = stats.used_pages();
    let free = stats.free_pages();
    let unused = stats.unused_pages();

    kprintln!("{heading}:");
    kprintln!(
        "  Total pages      : {: <10}({})",
        total,
        (total << page_size_shift).size_display_wide()
    );
    kprintln!(
        "    Metadata pages : {: <10}({})",
        meta,
        (meta << page_size_shift).size_display_wide()
    );
    kprintln!(
        "    Heap pages     : {: <10}({})",
        heap,
        (heap << page_size_shift).size_display_wide()
    );
    kprintln!(
        "      Used pages   : {: <10}({})",
        used,
        (used << page_size_shift).size_display_wide()
    );
    kprintln!(
        "      Free pages   : {: <10}({})",
        free,
        (free << page_size_shift).size_display_wide()
    );
    kprintln!(
        "    Unused pages   : {: <10}({})",
        unused,
        (unused << page_size_shift).size_display_wide()
    );
}
