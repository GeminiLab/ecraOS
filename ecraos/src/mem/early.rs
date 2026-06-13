//! Early (before vmm enabled) page allocator.

use core::{cell::UnsafeCell, mem};

use bitmaps::Bitmap;
use exarch::mem::{MemoryRegion, MemoryRegionFlags};
use expt::PagingHandler;
use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, VirtAddr, align_up};
use size_disp::SizeDisplay;

use crate::kprintln;

/// Early paging handler used before the virtual address space is ready.
pub struct EarlyPagingHandler;

impl PagingHandler for EarlyPagingHandler {
    fn alloc_page_aligned(bytes: usize) -> Option<PhysAddr> {
        unsafe { EARLY_PAGE_ALLOCATOR.alloc_page_aligned(bytes) }
    }

    fn dealloc_page_aligned(addr: PhysAddr, bytes: usize) {
        unsafe {
            EARLY_PAGE_ALLOCATOR.dealloc_page_aligned(addr, bytes);
        }
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
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

/// Destroys the early page allocator and returns its internal state.
///
/// Returns the allocation bitmap, page size shift, and base physical address.
pub fn destroy_early_page_allocator() -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, usize, PhysAddr) {
    unsafe { EARLY_PAGE_ALLOCATOR.destroy() }
}

pub fn alloc_page_aligned(bytes_required: usize) -> Option<PhysAddr> {
    unsafe { EARLY_PAGE_ALLOCATOR.alloc_page_aligned(bytes_required) }
}

#[expect(dead_code)]
pub fn dealloc_page_aligned(addr: PhysAddr, bytes_allocated: usize) {
    unsafe { EARLY_PAGE_ALLOCATOR.dealloc_page_aligned(addr, bytes_allocated) }
}

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

    /// Allocates a contiguous block of pages that is at least `bytes_required`
    /// bytes large.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn alloc_page_aligned(&self, bytes: usize) -> Option<PhysAddr> {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };
        let pages_required = align_up(bytes, 1usize << page_size_shift) >> page_size_shift;

        let mut start_index = 0;
        for index in 0..EARLY_PAGE_ALLOCATOR_PAGES {
            if bitmap.get(index) {
                // Used
                start_index = index + 1;
            } else {
                // Free
                if index - start_index + 1 == pages_required {
                    for p in start_index..=index {
                        bitmap.set(p, true);
                    }

                    return Some(base_paddr + (start_index << page_size_shift));
                }
            }
        }

        None
    }

    /// Deallocates a contiguous block of pages that was allocated by the early
    /// page allocator.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the early page
    /// allocator is happening.
    pub unsafe fn dealloc_page_aligned(&self, addr: PhysAddr, bytes: usize) {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };

        let start_index = (addr - base_paddr) >> page_size_shift;
        let pages_allocated = align_up(bytes, 1usize << page_size_shift) >> page_size_shift;

        for index in start_index..start_index + pages_allocated {
            bitmap.set(index, false);
        }
    }

    pub fn phys_addr_range(&self) -> PhysAddrRange {
        let (_, page_shift, base_paddr) = unsafe { self.assert_inited() };

        PhysAddrRange::from_start_size(base_paddr, EARLY_PAGE_ALLOCATOR_PAGES << page_shift)
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
