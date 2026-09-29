//! The physical page allocator.

use exarch::mem::MemoryRegionFlags;
use exbuddy::{AllocatorStats, BuddyAllocator};
use kspin::SpinNoIrq;
use log::{debug, info};
use memory_addr::{AddrRangeBounds, PhysAddr};
use size_disp::SizeDisplay;

use crate::{
    kprintln,
    mem::{early, pmm, vmm},
};

/// The global buddy allocator.
static BUDDY: SpinNoIrq<BuddyAllocator> = SpinNoIrq::new(BuddyAllocator::new());

/// Initializes the page allocator.
pub fn init_palloc() {
    let phys_regions = pmm::phys_mem_regions();
    let page_size_shift = vmm::page_size_shift();
    let page_size = vmm::page_size();
    let dmo = vmm::direct_mapping_offset();

    info!("Initializing page allocator...");

    let early_alloc_range = early::phys_addr_range();
    let (early_alloc_bitmap, _, early_allocbase_paddr) = early::destroy_early_page_allocator();

    debug!(
        "Early page allocator at {:x} destroyed, {} pages allocated",
        early_alloc_range.start,
        early_alloc_bitmap.len()
    );

    let mut buddy = BUDDY.lock();
    // SAFETY: we believe that we have mapped the physical memory to the virtual memory correctly.
    unsafe { buddy.init(page_size_shift, dmo) }.expect("failed to init buddy allocator");

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

/// Runs a function with the buddy allocator locked.
pub fn with_allocator<F: FnOnce(&mut BuddyAllocator) -> R, R>(f: F) -> R {
    let mut buddy = BUDDY.lock();
    f(&mut buddy)
}

/// Allocates a single physical frame.
pub fn alloc_frame() -> exbuddy::BuddyResult<PhysAddr> {
    with_allocator(|b| b.alloc_frame())
}

/// Allocates exactly `frame_count` contiguous physical frames.
///
/// The returned frame range is aligned to at least `align` bytes. The same
/// `frame_count` must be passed back to [`dealloc_frames`].
pub fn alloc_frames(frame_count: usize, align: usize) -> exbuddy::BuddyResult<PhysAddr> {
    let page_size = vmm::page_size();
    with_allocator(|b| b.alloc_frames(frame_count, align.max(page_size)))
}

/// Allocates exactly `frame_count` physical frames at `addr`.
///
/// The same `addr` and `frame_count` must be passed back to
/// [`dealloc_frames_at`].
#[expect(unused)]
pub fn alloc_frames_at(addr: PhysAddr, frame_count: usize) -> exbuddy::BuddyResult<PhysAddr> {
    with_allocator(|b| b.alloc_frames_at(addr, frame_count).map(|_| addr))
}

/// Deallocates a single physical frame allocated with [`alloc_frame`].
pub fn dealloc_frame(addr: PhysAddr) -> exbuddy::BuddyResult {
    with_allocator(|b| b.dealloc_frame(addr))
}

/// Deallocates an exact frame range allocated with [`alloc_frames`].
///
/// The `addr` and `frame_count` arguments must match the corresponding
/// [`alloc_frames`] call exactly.
pub fn dealloc_frames(addr: PhysAddr, frame_count: usize) -> exbuddy::BuddyResult {
    with_allocator(|b| b.dealloc_frames(addr, frame_count))
}

/// Deallocates an exact frame range allocated with [`alloc_frames_at`].
///
/// The `addr` and `frame_count` arguments must match the corresponding
/// [`alloc_frames_at`] call exactly.
#[expect(unused)]
pub fn dealloc_frames_at(addr: PhysAddr, frame_count: usize) -> exbuddy::BuddyResult {
    with_allocator(|b| b.dealloc_frames_at(addr, frame_count))
}

/// Returns whether the page containing `addr` is allocated.
#[expect(unused)]
pub fn is_allocated(addr: PhysAddr) -> exbuddy::BuddyResult<bool> {
    with_allocator(|b| b.is_allocated(addr))
}

/// Returns aggregate buddy allocator statistics.
pub fn stats() -> AllocatorStats {
    with_allocator(|b| b.stats())
}

/// Prints aggregate buddy allocator statistics.
pub fn print_buddy_stats(heading: &str) {
    let stats = stats();
    let page_size_shift = vmm::page_size_shift();
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
