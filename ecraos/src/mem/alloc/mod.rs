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
//! Call [`init_allocators`] once during boot (after VMM is enabled and the
//! early page allocator has been destroyed). Before that, any attempt to
//! allocate through the global allocator will panic.
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

use exbuddy::{AllocatorUsage, BuddyAllocator};
use exslab::{
    SlabAllocResult, SlabPoolDeallocResult, SlabPoolTrait,
    page::SlabPageHeader,
    slab::{PerCpuSlab, StaticSlabPool},
};
use kspin::SpinNoIrq;
use memory_addr::{PhysAddr, PhysAddrRange, VirtAddr};

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
static mut BUDDY: SpinNoIrq<BuddyAllocator> = SpinNoIrq::new(BuddyAllocator::new());

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

// ---------------------------------------------------------------------------
// Raw-pointer accessors for statics (edition 2024 safe access)
// ---------------------------------------------------------------------------

/// Returns a reference to the buddy allocator spinlock.
///
/// # Panics
///
/// Panics if called before [`init_allocators`] has run.
fn buddy() -> &'static SpinNoIrq<BuddyAllocator> {
    // SAFETY: the buddy allocator is initialized once during boot and never
    // moved or mutated outside of the lock. Using a raw pointer avoids the
    // edition-2024 `static_mut_refs` lint.
    unsafe { (&raw mut BUDDY).as_ref().expect("BUDDY not initialized") }
}

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
                let paddr = match buddy().lock().alloc_frames(pages, page_size) {
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

    match buddy().lock().alloc_frames(pages, align) {
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
            buddy().lock().dealloc_frames(paddr, pages);
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
    buddy().lock().dealloc_frame(paddr);
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

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initializes the kernel allocators.
///
/// Sets up the buddy allocator with usable memory regions (excluding the
/// kernel image and the early page allocator region), creates the slab pool,
/// and switches the global allocator from the panicking stub to the real one.
///
/// # Safety
///
/// Must be called exactly once, after the VMM is enabled and
/// [`mem::init_vmm_later`] has been called (so the early allocator range is
/// available). The caller must ensure that no other CPU is concurrently
/// accessing memory.
pub unsafe fn init_allocators(boot_arg: *const exboot::BootArg) {
    let mem_regions =
        unsafe { explat::mem::boot_mem_regions(boot_arg.as_ref_unchecked().plat_arg) }
            .expect("memory info unavailable in init_allocators");

    let page_size = 1usize << mem::vmm::page_size_shift();
    let vpo = mem::vmm::virt_phys_offset();

    // Regions to exclude from the buddy allocator.
    let kernel_phys_range = {
        let kr = mem::sections::kernel_range();
        PhysAddrRange::new(
            PhysAddr::from_usize(kr.start.as_usize() - vpo),
            PhysAddr::from_usize(kr.end.as_usize() - vpo),
        )
    };
    let early_range = mem::early::early_allocator_range();

    // The boot stack is still in use (kernel runs on it), so it must be excluded.
    let boot_stack_range = mem::boot_stack_range();

    kprintln!("Initializing allocators ...");

    // Excluded physical ranges (kernel image + early allocator + boot stack).
    let excluded = [kernel_phys_range, early_range, boot_stack_range];

    // Collect usable sub-regions, carving out excluded ranges.
    let mut first = true;
    for region in &mem_regions {
        if !matches!(region.type_, explat::mem::BootMemoryRegionType::Usable) {
            continue;
        }

        // For each usable region, produce sub-ranges that don't overlap
        // any excluded range. We do this by iteratively trimming.
        let mut sub_regions: [Option<PhysAddrRange>; 8] =
            [Some(region.range), None, None, None, None, None, None, None];
        for excl in &excluded {
            let mut next: [Option<PhysAddrRange>; 8] = [None; 8];
            let mut count = 0usize;
            for sr in sub_regions.into_iter().flatten() {
                if !sr.overlaps(*excl) {
                    next[count] = Some(sr);
                    count += 1;
                } else {
                    if sr.start < excl.start {
                        let end = excl.start.min(sr.end);
                        let left = PhysAddrRange::new(sr.start, end);
                        if left.size() > 0 {
                            next[count] = Some(left);
                            count += 1;
                        }
                    }
                    if sr.end > excl.end {
                        let start = excl.end.max(sr.start);
                        let right = PhysAddrRange::new(start, sr.end);
                        if right.size() > 0 {
                            next[count] = Some(right);
                            count += 1;
                        }
                    }
                }
            }
            sub_regions = next;
        }

        for sub in sub_regions.into_iter().flatten() {
            if sub.size() < page_size * 4 {
                continue;
            }
            if first {
                unsafe {
                    buddy()
                        .lock()
                        .init(sub, page_size, vpo)
                        .expect("failed to init buddy allocator with first region");
                }
                first = false;
                kprintln!("  Buddy init with region: {:x}", sub);
            } else {
                unsafe {
                    buddy()
                        .lock()
                        .add_region(sub)
                        .expect("failed to add region to buddy allocator");
                }
                kprintln!("  Buddy added region: {:x}", sub);
            }
        }
    }

    assert!(!first, "no usable memory region found for buddy allocator");

    // Create the slab pool (1 CPU, cpu_id = 0).
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

    kprintln!(
        "  Allocators initialized (page_size={}, virt_phys_offset={:#x})",
        page_size,
        vpo
    );
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Allocates a single physical frame.
///
/// Delegates directly to the buddy allocator.
pub fn alloc_frame() -> exbuddy::AllocResult<PhysAddr> {
    buddy().lock().alloc_frame()
}

/// Allocates `count` contiguous physical frames with the given alignment.
///
/// Delegates directly to the buddy allocator.
pub fn alloc_frames(count: usize, align: usize) -> exbuddy::AllocResult<PhysAddr> {
    let page_size = 1usize << mem::vmm::page_size_shift();
    buddy().lock().alloc_frames(count, align.max(page_size))
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
pub unsafe fn alloc_frames_at(paddr: PhysAddr, count: usize) -> exbuddy::AllocResult<PhysAddr> {
    // SAFETY: the buddy allocator has been initialized. The caller guarantees
    // that `paddr` and `paddr + count * page_size` are valid.
    unsafe { buddy().lock().alloc_frames_at(paddr, count) }
}

/// Deallocates `count` contiguous physical frames starting at `addr`.
///
/// Delegates directly to the buddy allocator.
pub fn dealloc_frames(addr: PhysAddr, count: usize) {
    buddy().lock().dealloc_frames(addr, count)
}

/// Deallocates a single physical frame.
///
/// Delegates directly to the buddy allocator.
pub fn dealloc_frame(addr: PhysAddr) {
    buddy().lock().dealloc_frame(addr)
}

/// Returns usage statistics for the buddy allocator.
pub fn usage() -> AllocatorUsage {
    buddy().lock().usage()
}
