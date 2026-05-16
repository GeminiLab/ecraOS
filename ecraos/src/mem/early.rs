//! Early (before vmm enabled) page allocator.

use core::{cell::UnsafeCell, mem};

use bitmaps::Bitmap;
use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, VirtAddr, align_up};

use explat::mem::{BootMemoryRegionType, BootMemoryRegions};
use expt::PagingHandler;
use size_disp::SizeDisplay;

use crate::kprintln;

/// Early paging handler used before the virtual address space is ready.
pub struct EarlyPagingHandler;

impl PagingHandler for EarlyPagingHandler {
    fn alloc_frames(bytes_required: usize) -> Option<PhysAddr> {
        unsafe { EARLY_PAGE_ALLOCATOR.alloc_pages(bytes_required) }
    }

    fn dealloc_frames(addr: PhysAddr, bytes_allocated: usize) {
        unsafe {
            EARLY_PAGE_ALLOCATOR.dealloc_pages(addr, bytes_allocated);
        }
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        addr.as_usize().into()
    }
}

static EARLY_PAGE_ALLOCATOR: EarlyPageAllocator = EarlyPageAllocator::new_uninit();

pub fn init_early_page_allocator(base_paddr: PhysAddr) {
    let page_size_shift = super::vmm::page_size_shift();
    kprintln!(
        "Initializing early page allocator at {:x} ({})\n",
        base_paddr,
        (EARLY_PAGE_ALLOCATOR_PAGES << page_size_shift).size_display_wide()
    );
    unsafe {
        EARLY_PAGE_ALLOCATOR.init(base_paddr, page_size_shift);
    }
}

pub fn destroy_early_page_allocator() -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, usize, PhysAddr) {
    unsafe { EARLY_PAGE_ALLOCATOR.destroy() }
}

const EARLY_PAGE_ALLOCATOR_PAGES: usize = 512;

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

    /// Initialize the early page allocator.
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
    pub unsafe fn alloc_pages(&self, bytes_required: usize) -> Option<PhysAddr> {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };
        let pages_required = align_up(bytes_required, 1usize << page_size_shift) >> page_size_shift;

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
    pub unsafe fn dealloc_pages(&self, addr: PhysAddr, bytes_allocated: usize) {
        // SAFETY: The caller promises it.
        let (bitmap, page_size_shift, base_paddr) = unsafe { self.assert_inited() };

        let start_index = (addr - base_paddr) >> page_size_shift;
        let pages_allocated =
            align_up(bytes_allocated, 1usize << page_size_shift) >> page_size_shift;

        for index in start_index..start_index + pages_allocated {
            bitmap.set(index, false);
        }
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

/// The physical address range that was occupied by the early page allocator.
///
/// Set by [`set_early_allocator_range`] before the allocator is destroyed,
/// read by the integration allocator module to exclude this range from the
/// buddy allocator.
static mut EARLY_ALLOCATOR_RANGE: Option<PhysAddrRange> = None;

/// Stores the early allocator range for later retrieval.
///
/// Called once during boot before the early allocator is destroyed.
pub fn set_early_allocator_range(range: PhysAddrRange) {
    unsafe {
        core::ptr::write(core::ptr::addr_of_mut!(EARLY_ALLOCATOR_RANGE), Some(range));
    }
}

/// Returns the early allocator range that was stored previously.
///
/// Panics if [`set_early_allocator_range`] has not been called yet.
pub fn early_allocator_range() -> PhysAddrRange {
    unsafe { (*core::ptr::addr_of!(EARLY_ALLOCATOR_RANGE)).expect("early allocator range not set") }
}

pub fn find_early_page_allocator_range(memory_info: &BootMemoryRegions) -> Option<PhysAddrRange> {
    let page_size_shift = super::vmm::page_size_shift();
    let page_size = 1usize << page_size_shift;
    let total_size = EARLY_PAGE_ALLOCATOR_PAGES << page_size_shift;

    let kernel_range = super::sections::kernel_range();
    let kernel_range = PhysAddrRange::new(
        kernel_range.start.as_usize().into(),
        kernel_range.end.as_usize().into(),
    );

    for region in memory_info.into_iter().rev() {
        if region.type_ != BootMemoryRegionType::Usable {
            continue;
        }

        let start_aligned = region.range.start.align_up(page_size);
        let end_aligned = region.range.end.align_down(page_size);
        let mut current_range =
            PhysAddrRange::from_start_size(end_aligned - total_size, total_size);

        while current_range.start >= start_aligned {
            if !current_range.overlaps(kernel_range) {
                return Some(current_range);
            }

            current_range.start -= total_size;
            current_range.end -= total_size;
        }
    }

    None
}
