//! Early stage memory management.

use core::{cell::UnsafeCell, mem};

use bitmaps::Bitmap;
use exarch::mem::{MemoryRegion, MemoryRegionFlags};
use expalloc_trait::PageAllocator;
use lazyinit::LazyInit;
use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};
use size_disp::SizeDisplay;

use crate::kprintln;

/// Early page allocator implementation used before the virtual address space is ready.
pub struct EarlyPageAllocatorImpl;

impl PageAllocator for EarlyPageAllocatorImpl {
    fn page_size_shift() -> usize {
        EARLY_PAGE_ALLOCATOR.page_size_shift()
    }

    fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
        alloc_frames(page_count)
    }

    fn dealloc_frames(phys_addr: PhysAddr, page_count: usize) {
        dealloc_frames(phys_addr, page_count);
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        // Early allocator is only used when identical mapping is used.
        addr.as_usize().into()
    }
}

static EARLY_PAGE_ALLOCATOR: EarlyPageAllocator = EarlyPageAllocator::new_uninit();

/// Initializes the early page allocator at the given physical base address.
pub fn init_early_page_allocator() {
    let range = find_early_page_allocator_range(super::pmm::phys_mem_regions())
        .expect("Cannot find early page allocator range");
    let base_paddr = range.start;

    let page_size_shift = super::vmm::page_size_shift();
    kprintln!(
        "Early page allocator:\n  Range: {:#x}\n  Size : {} ({} pages, {} each)\n",
        range,
        (EARLY_PAGE_ALLOCATOR_PAGES << page_size_shift).size_display_wide(),
        EARLY_PAGE_ALLOCATOR_PAGES,
        (1usize << page_size_shift).size_display_wide(),
    );
    unsafe {
        EARLY_PAGE_ALLOCATOR.init(base_paddr, page_size_shift);
    }
}

/// Finds a suitable range for the early page allocator within FREE regions.
///
/// This function finds the last suitable range for the early page allocator, to
/// avoid collisions with the metadata regions of the buddy page allocator.
fn find_early_page_allocator_range(regions: &[MemoryRegion]) -> Option<PhysAddrRange> {
    let page_size_shift = super::vmm::page_size_shift();
    let page_size = 1usize << page_size_shift;
    let total_size = EARLY_PAGE_ALLOCATOR_PAGES << page_size_shift;

    for region in regions.iter().rev() {
        if !region.flags.contains(MemoryRegionFlags::FREE) {
            continue;
        }

        let start_aligned = region.range.start.align_up(page_size);
        let end_aligned = region.range.end.align_down(page_size);

        if end_aligned
            .as_usize()
            .saturating_sub(start_aligned.as_usize())
            < total_size
        {
            continue;
        }

        let current_range = PhysAddrRange::from_start_size(end_aligned - total_size, total_size);
        return Some(current_range);
    }

    None
}

/// Destroys the early page allocator and returns its internal state.
///
/// Returns the allocation bitmap, page size shift, and base physical address.
pub fn destroy_early_page_allocator() -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, usize, PhysAddr) {
    unsafe { EARLY_PAGE_ALLOCATOR.destroy() }
}

/// Allocates a contiguous block of frames from the early page allocator.
#[inline]
pub fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
    unsafe { EARLY_PAGE_ALLOCATOR.alloc_frames(page_count) }
}

/// Deallocates a contiguous block of frames to the early page allocator.
#[inline]
pub fn dealloc_frames(phys_addr: PhysAddr, page_count: usize) {
    unsafe { EARLY_PAGE_ALLOCATOR.dealloc_frames(phys_addr, page_count) }
}

/// Returns the physical address range of the early page allocator.
#[inline]
pub fn phys_addr_range() -> PhysAddrRange {
    EARLY_PAGE_ALLOCATOR.phys_addr_range()
}

/// The number of pages allocated by the early page allocator.
pub const EARLY_PAGE_ALLOCATOR_PAGES: usize = 512;

#[derive(Debug)]
enum EarlyPageAllocatorInner {
    Uninit,
    Inited {
        bitmap: Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>,
        page_size_shift: usize,
        base_paddr: PhysAddr,
    },
    Destroyed,
}

#[derive(Debug)]
struct EarlyPageAllocator {
    inner: UnsafeCell<EarlyPageAllocatorInner>,
}

// Actually not, just to make the compiler happy
unsafe impl Sync for EarlyPageAllocator {}
unsafe impl Send for EarlyPageAllocator {}

impl EarlyPageAllocator {
    pub const fn new_uninit() -> Self {
        Self {
            inner: UnsafeCell::new(EarlyPageAllocatorInner::Uninit),
        }
    }

    /// Initializes the early page allocator.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn init(&self, base_paddr: PhysAddr, page_size_shift: usize) {
        // SAFETY: The caller promises it.
        if !matches!(
            unsafe { self.inner.get().as_ref_unchecked() },
            EarlyPageAllocatorInner::Uninit
        ) {
            panic!("Early page allocator already initialized");
        }

        unsafe {
            self.inner.get().write(EarlyPageAllocatorInner::Inited {
                bitmap: Bitmap::new(),
                page_size_shift,
                base_paddr,
            })
        };
    }

    /// Destroys the early page allocator and returns its internal state.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn destroy(&self) -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, usize, PhysAddr) {
        // SAFETY: The caller promises it.
        let original_value = mem::replace(
            unsafe { self.inner.get().as_mut_unchecked() },
            EarlyPageAllocatorInner::Destroyed,
        );

        match original_value {
            EarlyPageAllocatorInner::Inited {
                bitmap,
                page_size_shift,
                base_paddr,
            } => (bitmap, page_size_shift, base_paddr),
            EarlyPageAllocatorInner::Uninit => panic!("Early page allocator not initialized"),
            EarlyPageAllocatorInner::Destroyed => panic!("Early page allocator already destroyed"),
        }
    }

    /// Allocates a contiguous block of frames.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn alloc_frames(&self, page_count: usize) -> Option<PhysAddr> {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };

        let mut start_index = 0;
        for index in 0..EARLY_PAGE_ALLOCATOR_PAGES {
            if bitmap.get(index) {
                // Used
                start_index = index + 1;
            } else {
                // Free
                if index - start_index + 1 == page_count {
                    for p in start_index..=index {
                        bitmap.set(p, true);
                    }

                    return Some(base_paddr + (start_index << page_size_shift));
                }
            }
        }

        None
    }

    /// Deallocates a contiguous block of pages.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn dealloc_frames(&self, addr: PhysAddr, page_count: usize) {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };
        let start_index = (addr - base_paddr) >> page_size_shift;

        for index in start_index..start_index + page_count {
            bitmap.set(index, false);
        }
    }

    /// Returns the physical address range of the early page allocator.
    pub fn phys_addr_range(&self) -> PhysAddrRange {
        let (_, page_shift, base_paddr) = unsafe { self.assert_inited() };

        PhysAddrRange::from_start_size(base_paddr, EARLY_PAGE_ALLOCATOR_PAGES << page_shift)
    }

    /// Returns the page size shift of the early page allocator.
    pub fn page_size_shift(&self) -> usize {
        let (_, page_shift, _) = unsafe { self.assert_inited() };
        page_shift
    }

    /// Asserts that the early page allocator is initialized and returns a
    /// mutable reference to the bitmap and the base physical address. Panics if
    /// the early page allocator is not initialized or has been destroyed.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    #[allow(clippy::mut_from_ref)]
    unsafe fn assert_inited(&self) -> (&mut Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, usize, PhysAddr) {
        // SAFETY: The caller promises it.
        match unsafe { self.inner.get().as_mut_unchecked() } {
            EarlyPageAllocatorInner::Uninit => panic!("Early page allocator not initialized"),
            EarlyPageAllocatorInner::Inited {
                bitmap,
                page_size_shift,
                base_paddr,
            } => (bitmap, *page_size_shift, *base_paddr),
            EarlyPageAllocatorInner::Destroyed => panic!("Early page allocator destroyed"),
        }
    }
}

static BSP_STACK: LazyInit<(VirtAddrRange, VirtAddrRange, PhysAddrRange)> = LazyInit::new();

pub const BSP_STACK_SIZE: usize = 16 * 1024;

pub fn init_bsp_stack(base: VirtAddr) {
    let page_size_shift = EarlyPageAllocatorImpl::page_size_shift();
    let page_size = 1usize << page_size_shift;

    debug_assert!(base.is_aligned(page_size));

    let frame_count = BSP_STACK_SIZE.div_ceil(page_size);

    let guard_start = base;
    let real_start = guard_start + page_size;
    let real_end = real_start + frame_count * page_size;
    let guard_end = real_end + page_size;

    let full_range = VirtAddrRange::new(guard_start, guard_end);
    let alloc_range = VirtAddrRange::new(real_start, real_end);

    let start_pa =
        EarlyPageAllocatorImpl::alloc_frames(frame_count).expect("failed to allocate kernel stack");
    let end_pa = start_pa + frame_count * page_size;
    let pa_range = PhysAddrRange::new(start_pa, end_pa);

    BSP_STACK.init_once((full_range, alloc_range, pa_range));
}

pub fn bsp_stack() -> (VirtAddrRange, VirtAddrRange, PhysAddrRange) {
    *BSP_STACK
}
