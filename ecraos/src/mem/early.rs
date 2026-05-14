use core::{
    cell::UnsafeCell,
    mem,
    sync::atomic::{AtomicUsize, Ordering},
};

use bitmaps::Bitmap;
use memory_addr::{MemoryAddr, PhysAddr, VirtAddr, align_up};

use expt::PagingHandler;

use crate::early_println;

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

/// Early paging handler used when unmapping the identity map after switching to
/// the direct map area.
///
/// It's basically the same as the [`EarlyPagingHandler`], but it will
/// translate the physical addresses to virtual addresses using an offset.
pub struct NotVeryEarlyPagingHandler;

static VA_OFFSET: AtomicUsize = AtomicUsize::new(0);

impl NotVeryEarlyPagingHandler {
    pub fn set_offset(base: VirtAddr) {
        VA_OFFSET.store(base.as_usize(), Ordering::Relaxed);
    }
}

impl PagingHandler for NotVeryEarlyPagingHandler {
    fn alloc_frames(bytes_required: usize) -> Option<PhysAddr> {
        EarlyPagingHandler::alloc_frames(bytes_required)
    }

    fn dealloc_frames(addr: PhysAddr, bytes_allocated: usize) {
        EarlyPagingHandler::dealloc_frames(addr, bytes_allocated);
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        let offset = VA_OFFSET.load(Ordering::Relaxed);
        VirtAddr::from_usize(addr.into()) + offset
    }
}

static EARLY_PAGE_ALLOCATOR: EarlyPageAllocator = EarlyPageAllocator::new_uninit();

pub fn init_early_page_allocator(base_paddr: PhysAddr) {
    unsafe {
        EARLY_PAGE_ALLOCATOR.init(base_paddr);
    }
}

pub fn destroy_early_page_allocator() -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, PhysAddr) {
    unsafe { EARLY_PAGE_ALLOCATOR.destroy() }
}

const PAGE_SIZE: usize = 4096;
pub const EARLY_PAGE_ALLOCATOR_SIZE: usize = 2 * 1024 * 1024; // 2MiB
pub const EARLY_PAGE_ALLOCATOR_PAGES: usize = EARLY_PAGE_ALLOCATOR_SIZE / PAGE_SIZE;

#[derive(Debug)]
enum EarlyPageAllocatorInner {
    Uninit,
    Inited {
        bitmap: Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>,
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
    pub unsafe fn init(&self, base_paddr: PhysAddr) {
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
                base_paddr,
            })
        };
    }

    pub unsafe fn destroy(&self) -> (Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, PhysAddr) {
        // SAFETY: The caller promises it.
        let original_value = mem::replace(
            unsafe { self.inner.get().as_mut_unchecked() },
            EarlyPageAllocatorInner::Destroyed,
        );

        match original_value {
            EarlyPageAllocatorInner::Inited { bitmap, base_paddr } => (bitmap, base_paddr),
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
        let (bitmap, base_paddr) = unsafe { self.assert_inited() };
        let pages_required = align_up(bytes_required, PAGE_SIZE) / PAGE_SIZE;

        explat::dbcn_println!(
            "alloc_pages: bytes_required = {}, pages_required = {}",
            bytes_required,
            pages_required
        );

        let mut start = 0;
        for index in 0..EARLY_PAGE_ALLOCATOR_PAGES {
            if bitmap.get(index) {
                // Used
                start = index + 1;
            } else {
                // Free
                if index - start + 1 == pages_required {
                    for p in start..=index {
                        bitmap.set(p, true);
                    }

                    explat::dbcn_println!(
                        "alloc_pages: found block of {} pages at index {} ({:x})",
                        pages_required,
                        start,
                        base_paddr + start * PAGE_SIZE
                    );
                    return Some(base_paddr + start * PAGE_SIZE);
                }
            }
        }

        explat::dbcn_println!("alloc_pages: no block of {} pages found", pages_required);
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
        let (bitmap, base_paddr) = unsafe { self.assert_inited() };

        early_println!(
            "dealloc_pages: addr = {:x}, bytes_allocated = {}",
            addr,
            bytes_allocated
        );

        let start_index = (addr - base_paddr) / PAGE_SIZE;
        let pages_allocated = align_up(bytes_allocated, PAGE_SIZE) / PAGE_SIZE;

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
    unsafe fn assert_inited(&self) -> (&mut Bitmap<EARLY_PAGE_ALLOCATOR_PAGES>, PhysAddr) {
        // SAFETY: The caller promises it.
        match unsafe { self.inner.get().as_mut_unchecked() } {
            EarlyPageAllocatorInner::Uninit => panic!("Early page allocator not initialized"),
            EarlyPageAllocatorInner::Inited { bitmap, base_paddr } => (bitmap, *base_paddr),
            EarlyPageAllocatorInner::Destroyed => panic!("Early page allocator destroyed"),
        }
    }
}
