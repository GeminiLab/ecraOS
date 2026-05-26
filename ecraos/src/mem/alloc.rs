//! Kernel heap allocator integrating buddy and slab allocators.
//!
//! This module wires together the [`exbuddy::BuddyAllocator`] for page-level
//! physical memory management and the [`exslab`] slab allocator for small
//! objects, and exposes them through the Rust [`GlobalAlloc`] trait so that
//! `alloc` crate types (`Vec`, `Box`, `String`, etc.) become usable in the
//! kernel.
//!
//! # Initialization
//!
//! Call [`init_allocators`] once during boot (after VMM is enabled).
//! Before that, any attempt to allocate through the global allocator will
//! panic.
//!
//! # Lock ordering
//!
//! The slab and buddy allocators each have their own internal spinlock.
//! To prevent deadlocks the following ordering is respected:
//!
//! 1. Normal slab local path: only the slab lock is held.
//! 2. Normal buddy page path: only the buddy lock is held.
//! 3. Slab needs a new page (`NeedsSlab`): the slab lock is released, the
//!    buddy lock is acquired and released, then the slab lock is re-acquired
//!    to add the slab page.
//! 4. Slab returns an empty page (`FreeSlab`): the slab lock is released,
//!    then the buddy lock is acquired and released.

extern crate alloc;

use core::{
    alloc::{GlobalAlloc, Layout},
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use exbuddy::{AllocatorStats, BuddyAllocator};
use explat::mem::MemoryRegionFlags;
use exslab::{
    SlabAllocResult, SlabPoolDeallocResult, SlabPoolTrait,
    page::SlabPageHeader,
    slab::{PerCpuSlab, StaticSlabPool},
};
use kspin::SpinNoIrq;
use memory_addr::{PhysAddr, VirtAddr};

use crate::{kprintln, mem};

mod test;

// ---------------------------------------------------------------------------
// Static storage
// ---------------------------------------------------------------------------

/// Whether the allocator has been initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// The buddy allocator, protected by a spinlock.
///
/// Written once during [`init_allocators`] and accessed thereafter through
/// the lock.
static BUDDY: SpinNoIrq<BuddyAllocator> = SpinNoIrq::new(BuddyAllocator::new());

/// Backing storage for the slab pool.
///
/// Written once during [`init_allocators`] via `MaybeUninit::write` and
/// accessed thereafter through [`SLAB_POOL`].
static mut SLAB_POOL_STORAGE: MaybeUninit<StaticSlabPool<1>> = MaybeUninit::uninit();

/// Reference to the initialized slab pool.
///
/// Set once during [`init_allocators`].
static mut SLAB_POOL: Option<&'static StaticSlabPool<1>> = None;

/// The virtual-to-physical address offset used for address conversion.
///
/// Set once during [`init_allocators`].
static mut VIRT_PHYS_OFFSET: usize = 0;

/// Returns the slab pool reference, panicking if not initialized.
fn slab_pool() -> &'static StaticSlabPool<1> {
    unsafe {
        (&raw const SLAB_POOL)
            .as_ref()
            .expect("SLAB_POOL pointer")
            .expect("slab pool not initialized")
    }
}

/// Returns the stored virtual-to-physical offset.
fn virt_phys_offset() -> usize {
    // SAFETY: only read after initialization.
    unsafe { core::ptr::read(core::ptr::addr_of!(VIRT_PHYS_OFFSET)) }
}

// ---------------------------------------------------------------------------
// Internal helpers (used by GlobalAlloc)
// ---------------------------------------------------------------------------

/// Allocates a small object (<= 2048 bytes) through the slab allocator.
///
/// When the slab returns `NeedsSlab`, this function allocates pages from the
/// buddy allocator, adds them as a slab page, and retries.
fn alloc_small(layout: Layout) -> *mut u8 {
    let pool = slab_pool();
    let page_size = 1usize << mem::vmm::page_size_shift();
    let vpo = virt_phys_offset();

    loop {
        match pool.alloc(layout) {
            Ok(SlabAllocResult::Allocated(ptr)) => return ptr.as_ptr(),
            Ok(SlabAllocResult::NeedsSlab { size_class, pages }) => {
                // Slab lock is released. Safe to acquire buddy lock.
                let paddr = match BUDDY.lock().alloc_frames(pages, page_size) {
                    Ok(p) => p,
                    Err(_) => return core::ptr::null_mut(),
                };
                let vaddr = VirtAddr::from_usize(paddr.as_usize() + vpo);
                let bytes = pages * page_size;
                pool.add_slab(size_class, vaddr.as_usize(), bytes);
                // Loop back to retry allocation.
            }
            Err(_) => return core::ptr::null_mut(),
        }
    }
}

/// Allocates a large object (> 2048 bytes) through the buddy allocator.
///
/// Rounds the allocation up to whole pages and returns the virtual address
/// of the first page.
fn alloc_large(layout: Layout) -> *mut u8 {
    let page_size = 1usize << mem::vmm::page_size_shift();
    let vpo = virt_phys_offset();
    let bytes = layout.size().max(layout.align());
    let pages = bytes.div_ceil(page_size);
    let align = layout.align().max(page_size);

    match BUDDY.lock().alloc_frames(pages, align) {
        Ok(paddr) => (paddr.as_usize() + vpo) as *mut u8,
        Err(_) => core::ptr::null_mut(),
    }
}

/// Deallocates a small object through the slab allocator.
///
/// Handles the `FreeSlab` case by returning the empty slab page to the buddy
/// allocator.
fn dealloc_small(ptr: *mut u8, layout: Layout) {
    let pool = slab_pool();
    let page_size = 1usize << mem::vmm::page_size_shift();
    let vpo = virt_phys_offset();

    let obj_addr = ptr as usize;
    let owner_cpu = match SlabPageHeader::base_from_obj_addr_unknown(obj_addr, page_size) {
        Some(base) => unsafe { (*(base as *const SlabPageHeader)).owner_cpu as usize },
        None => return,
    };

    let nn_ptr = match NonNull::new(ptr) {
        Some(p) => p,
        None => return,
    };

    match pool.dealloc(nn_ptr, layout, owner_cpu) {
        SlabPoolDeallocResult::Done | SlabPoolDeallocResult::RemoteQueued => {}
        SlabPoolDeallocResult::FreeSlab { base, pages } => {
            let paddr = PhysAddr::from_usize(base.as_usize() - vpo);
            BUDDY.lock().dealloc_frames(paddr, pages);
        }
    }
}

/// Deallocates a large object through the buddy allocator.
///
/// The pointer is converted from virtual to physical before being returned
/// to the buddy allocator.
fn dealloc_large(ptr: *mut u8, _layout: Layout) {
    let vpo = virt_phys_offset();
    let paddr = PhysAddr::from_usize(ptr as usize - vpo);
    // The buddy allocator reads the stored order from the page metadata
    // so dealloc_frame is sufficient regardless of the original block size.
    BUDDY.lock().dealloc_frame(paddr);
}

// ---------------------------------------------------------------------------
// #[global_allocator]
// ---------------------------------------------------------------------------

/// The kernel global allocator.
///
/// Routes small objects (<= 2048 bytes with alignment <= 2048) through the
/// slab allocator and large objects through the buddy allocator. Panics if
/// called before [`init_allocators`] has set [`INITIALIZED`].
struct EcraosGlobalAlloc;

unsafe impl GlobalAlloc for EcraosGlobalAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !INITIALIZED.load(Ordering::Acquire) {
            panic!("allocator not initialized");
        }
        let effective_size = layout.size().max(layout.align());
        if effective_size <= 2048 && layout.align() <= 2048 {
            alloc_small(layout)
        } else {
            alloc_large(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !INITIALIZED.load(Ordering::Acquire) {
            panic!("allocator not initialized");
        }
        let effective_size = layout.size().max(layout.align());
        if effective_size <= 2048 && layout.align() <= 2048 {
            dealloc_small(ptr, layout)
        } else {
            dealloc_large(ptr, layout)
        }
    }
}

/// The global allocator static.
///
/// Before initialization, all allocations go through `EcraosGlobalAlloc`
/// which checks [`INITIALIZED`] and panics. After [`init_allocators`] sets
/// the flag, allocations are routed to the real buddy+slab backends.
#[global_allocator]
static GLOBAL_ALLOCATOR: EcraosGlobalAlloc = EcraosGlobalAlloc;

/// Initializes the kernel allocators.
///
/// Destroys the early page allocator, then sets up the buddy allocator with
/// FREE regions from the final physical memory region table. In-use pages
/// from the early allocator are marked as allocated via
/// [`BuddyAllocator::alloc_frames_at`]. Finally, creates the slab pool.
pub fn init_allocators() {
    let phys_regions = mem::pmm::phys_mem_regions();
    let page_size = 1usize << mem::vmm::page_size_shift();
    let vpo = mem::vmm::virt_phys_offset();

    // Step 1: Destroy the early page allocator.
    let early_alloc_range = mem::early::phys_addr_range();
    let (early_alloc_bitmap, _, early_allocbase_paddr) = mem::early::destroy_early_page_allocator();

    // Step 2: Iterate FREE regions from the final region table and add to buddy.
    kprintln!("Initializing allocator, early page allocator destroyed:");
    let mut buddy = BUDDY.lock();
    // SAFETY: we believe that we have mapped the physical memory to the virtual
    // memory correctly.
    unsafe { buddy.init(page_size, vpo) }.expect("failed to init buddy allocator");

    for region in phys_regions {
        if !region.flags.contains(MemoryRegionFlags::FREE) {
            continue;
        }

        // Is this range used by the early allocator? If so, we need to check
        // whether the buddy metadata would overlap the early allocator range.
        if early_alloc_range.overlaps(region.range) {
            // Overlapping means containing here.
            if BuddyAllocator::check_metadata_overlap(region.range, page_size, early_alloc_range) {
                panic!(
                    "Buddy metadata overlaps early allocator for region {:x} ({})",
                    region.range, region.desc
                );
            }
        }

        kprintln!("  Allocator region: {:x} ({})", region.range, region.desc);
        // SATETY: we believe that the platform crate gives us a valid physical
        // memory map.
        unsafe { buddy.add_region(region.range) }
            .expect("failed to add the previous region to buddy allocator");
    }

    assert!(
        buddy.section_count() > 0,
        "no usable memory region found for buddy allocator"
    );

    // Step 3: Mark early allocator's in-use pages as allocated in the buddy.
    for page_index_early_allocated in &early_alloc_bitmap {
        unsafe {
            buddy
                .alloc_frames_at(
                    early_allocbase_paddr + (page_index_early_allocated * page_size),
                    1,
                )
                .expect("failed to mark early allocator page as in-use");
        }
    }
    let early_alloc_page_count = early_alloc_bitmap.len();
    kprintln!(
        "  Marked {} page allocated by the early allocator as in-use",
        early_alloc_page_count,
    );
    drop(buddy);

    // Step 4: Create the slab pool (1 CPU, cpu_id = 0).
    let slab_pool = StaticSlabPool::new([PerCpuSlab::new(0, page_size)], || 0, page_size);
    unsafe {
        // SAFETY: no concurrent access during single-CPU boot initialization.
        let storage = core::ptr::addr_of_mut!(SLAB_POOL_STORAGE);
        (*storage).write(slab_pool);
        let pool_ref = (*storage).assume_init_ref();
        core::ptr::addr_of_mut!(SLAB_POOL).write(Some(pool_ref));
        core::ptr::addr_of_mut!(VIRT_PHYS_OFFSET).write(vpo);
    }

    INITIALIZED.store(true, Ordering::Release);

    test::run();
}

/// Allocates a single physical frame.
///
/// Delegates directly to the buddy allocator.
pub fn alloc_frame() -> exbuddy::BuddyResult<PhysAddr> {
    BUDDY.lock().alloc_frame()
}

/// Allocates `count` contiguous physical frames with the given alignment.
///
/// Delegates directly to the buddy allocator.
pub fn alloc_frames(count: usize, align: usize) -> exbuddy::BuddyResult<PhysAddr> {
    let page_size = 1usize << mem::vmm::page_size_shift();
    BUDDY.lock().alloc_frames(count, align.max(page_size))
}

/// Allocates `count` contiguous physical frames starting at the given physical
/// address.
///
/// Delegates directly to the buddy allocator.
///
/// # Safety
///
/// The caller must ensure that `paddr` and `paddr + count * page_size` are
/// valid physical addresses within a managed region, and that no other
/// references to those frames exist.
pub unsafe fn alloc_frames_at(paddr: PhysAddr, count: usize) -> exbuddy::BuddyResult<PhysAddr> {
    // SAFETY: the buddy allocator has been initialized. The caller guarantees
    // that `paddr` and `paddr + count * page_size` are valid.
    unsafe { BUDDY.lock().alloc_frames_at(paddr, count) }
}

/// Deallocates `count` contiguous physical frames starting at `addr`.
///
/// Delegates directly to the buddy allocator.
pub fn dealloc_frames(addr: PhysAddr, count: usize) {
    BUDDY.lock().dealloc_frames(addr, count)
}

/// Deallocates a single physical frame.
///
/// Delegates directly to the buddy allocator.
pub fn dealloc_frame(addr: PhysAddr) {
    BUDDY.lock().dealloc_frame(addr)
}

/// Returns usage statistics for the buddy allocator.
pub fn stats() -> AllocatorStats {
    BUDDY.lock().stats()
}
