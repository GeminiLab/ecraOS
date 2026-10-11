//! Slab allocator core.
//!
//! The [`SlabAllocator`] is a standalone component that manages object allocation
//! within pre-supplied slab pages. It does **not** allocate pages itself; instead
//! it returns [`SlabAllocResult::NeedsSlab`] to request pages from the caller.
//!
//! Cross-CPU frees go through the lock-free [`SlabPageHeader::remote_free`] path.

use core::alloc::Layout;
use core::ptr::NonNull;

use memory_addr::{MemoryAddr, VirtAddr};

use crate::{
    cache::{CacheDeallocResult, SlabCache},
    error::{AllocError, AllocResult},
    page::SlabPageHeader,
    size_class::SizeClass,
};

/// Result of a slab allocation attempt.
pub enum SlabAllocResult {
    /// Object successfully allocated.
    Allocated(NonNull<u8>),
    /// The slab cache for this size class has no free objects.
    ///
    /// The caller should allocate `pages` pages from the buddy allocator,
    /// call [`SlabAllocator::add_slab`], and retry.
    NeedsSlab { size_class: SizeClass, pages: usize },
}

/// Result of a slab deallocation.
pub enum SlabDeallocResult {
    /// Object freed, nothing else to do.
    Done,
    /// The slab page at `base` became empty and should be returned to the caller.
    FreeSlab { base: VirtAddr, pages: usize },
}

/// Standalone slab allocator (one per CPU or standalone use).
pub struct SlabAllocator {
    /// Per-size-class slab caches.
    caches: [SlabCache; SizeClass::COUNT],
    /// Page size used by this allocator.
    page_size: usize,
}

impl SlabAllocator {
    /// Creates a new (empty) slab allocator for the given page size.
    ///
    /// No pages are owned yet.
    pub const fn new(page_size: usize) -> Self {
        Self {
            caches: [
                SlabCache::new(SizeClass::Bytes8),
                SlabCache::new(SizeClass::Bytes16),
                SlabCache::new(SizeClass::Bytes32),
                SlabCache::new(SizeClass::Bytes64),
                SlabCache::new(SizeClass::Bytes128),
                SlabCache::new(SizeClass::Bytes256),
                SlabCache::new(SizeClass::Bytes512),
                SlabCache::new(SizeClass::Bytes1024),
                SlabCache::new(SizeClass::Bytes2048),
            ],
            page_size,
        }
    }
}

impl Default for SlabAllocator {
    fn default() -> Self {
        Self::new(0x1000)
    }
}

impl SlabAllocator {
    /// Tries to allocate an object matching `layout`.
    ///
    /// If the matching cache is exhausted, [`SlabAllocResult::NeedsSlab`] is returned
    /// so the caller can supply pages and retry.
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult> {
        let sc = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let cache = &mut self.caches[sc.index()];

        match cache.alloc_object(self.page_size) {
            Some(addr) => {
                // SAFETY: `addr` is non-null, aligned, and within a live slab page.
                let ptr = unsafe { NonNull::new_unchecked(addr.as_mut_ptr()) };
                Ok(SlabAllocResult::Allocated(ptr))
            }
            None => Ok(SlabAllocResult::NeedsSlab {
                size_class: sc,
                pages: sc.slab_pages(self.page_size),
            }),
        }
    }

    /// Frees an object previously allocated with [`alloc`](Self::alloc).
    ///
    /// This is the **local** (owner-CPU) path. Cross-CPU frees should go through
    /// [`SlabPageHeader::remote_free`] directly.
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        let sc = SizeClass::from_layout(layout).expect("layout exceeds slab size");
        let cache = &mut self.caches[sc.index()];

        match cache.dealloc_object(self.page_size, VirtAddr::from_mut_ptr_of(ptr.as_ptr())) {
            CacheDeallocResult::Done => SlabDeallocResult::Done,
            CacheDeallocResult::FreeSlab { base, pages } => {
                SlabDeallocResult::FreeSlab { base, pages }
            }
        }
    }

    /// Supplies freshly allocated slab pages to the given size class.
    ///
    /// `base` is the address of the first page, and `bytes` is the total
    /// page-aligned span. The pages must be mapped and writable because the
    /// slab header and object data are stored in the supplied memory.
    pub fn add_slab(
        &mut self,
        size_class: SizeClass,
        base: VirtAddr,
        bytes: usize,
        owner_cpu: u16,
    ) -> AllocResult {
        if self.page_size == 0
            || !self.page_size.is_power_of_two()
            || base.as_usize() == 0
            || !base.is_aligned(self.page_size)
            || bytes == 0
            || !bytes.is_multiple_of(self.page_size)
            || bytes < SlabPageHeader::data_offset(size_class.size()) + size_class.size()
        {
            return Err(AllocError::InvalidParam);
        }

        self.caches[size_class.index()].add_slab(base, bytes, owner_cpu);
        Ok(())
    }
}
