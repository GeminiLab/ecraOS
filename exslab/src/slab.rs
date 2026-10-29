//! Slab allocation with runtime selection of precomputed layout data.
//!
//! Each size class binds an immutable geometry record. Cache and page operations share one
//! implementation, and backing allocation and serialization remain the caller's responsibility.

use core::{alloc::Layout, ptr::NonNull};

use memory_addr::VirtAddr;

use crate::{
    SlabLayout,
    cache::SlabCache,
    error::{AllocError, AllocResult},
    page::{SlabPage, SlabPageHeader},
    size_class::SizeClass,
};

/// Result of a slab allocation attempt.
pub enum SlabAllocResult {
    /// A successfully allocated object.
    Allocated(NonNull<u8>),
    /// Backing required before the caller retries allocation.
    ///
    /// The caller supplies exactly this many contiguous mapped writable pages aligned to the
    /// complete span, then registers them with the same allocator.
    NeedsSlab {
        /// Size class whose cache needs backing.
        size_class: SizeClass,
        /// Exact number of hardware pages in the requested slab.
        pages: usize,
    },
}

/// Result of a local slab deallocation.
pub enum SlabDeallocResult {
    /// An object released while its backing remains cached.
    Done,
    /// An excess empty slab removed from the cache for the caller to release.
    FreeSlab {
        /// Virtual base of the complete backing allocation.
        base: VirtAddr,
        /// Exact number of hardware pages to release.
        pages: usize,
    },
}

/// Allocator selecting geometry data while sharing all cache and page operation code.
///
/// The page size is fixed at construction to 4, 16, or 64 KiB. Backing sizes follow
/// `max(min(65536, object_size * 512), page_size)` and require full-span alignment.
/// The caller serializes owner operations and retains backing until returned or all users end.
/// Dropping an allocator does not release backing.
pub struct SlabAllocator {
    /// Independent class states, each permanently bound to a static layout.
    caches: [SlabCache; SizeClass::SIZE_CLASS_COUNT],
    /// Hardware page size used to report backing requests and returns.
    page_size: usize,
}

impl SlabAllocator {
    /// Creates an empty allocator after validating the hardware page size.
    pub fn new(page_size: usize) -> AllocResult<Self> {
        let page_log2 = Self::page_log2(page_size)?;
        Ok(Self {
            caches: core::array::from_fn(|index| {
                SlabCache::with_layout(SlabLayout::for_class(SizeClass::ALL[index], page_log2))
            }),
            page_size,
        })
    }

    /// Validates supported hardware pages and returns their exponent.
    fn page_log2(page_size: usize) -> AllocResult<usize> {
        match page_size {
            4096 => Ok(12),
            16384 => Ok(14),
            65536 => Ok(16),
            _ => Err(AllocError::InvalidParam),
        }
    }

    /// Allocates a matching slot or requests exact backing pages aligned to their complete span.
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult> {
        let class = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let cache = &mut self.caches[class.index()];
        match cache.alloc_object() {
            Some(address) => {
                // SAFETY: A successfully allocated cache slot has a non-null live address.
                let object = unsafe { NonNull::new_unchecked(address.as_mut_ptr()) };
                Ok(SlabAllocResult::Allocated(object))
            }
            None => Ok(SlabAllocResult::NeedsSlab {
                size_class: class,
                pages: cache.layout().slab_size / self.page_size,
            }),
        }
    }

    /// Registers complete backing for the selected class after checking full-span alignment.
    ///
    /// A raw backing address cannot be registered from safe code:
    ///
    /// ```compile_fail
    /// use exslab::{SizeClass, SlabAllocator};
    /// use memory_addr::va;
    ///
    /// let mut allocator = SlabAllocator::new(4096).unwrap();
    /// let _ = allocator.add_slab(SizeClass::Bytes8, va!(0x1000), 0);
    /// ```
    ///
    /// # Safety
    ///
    /// An accepted base must identify exclusive mapped writable backing of the requested span.
    /// It must stay live until returned, or until all cache operations, slots, and producers end.
    pub unsafe fn add_slab(
        &mut self,
        class: SizeClass,
        base: VirtAddr,
        owner_cpu: u16,
    ) -> AllocResult {
        // SAFETY: The caller supplies complete exclusive backing for this permanently bound cache.
        unsafe { self.caches[class.index()].add_slab(base, owner_cpu) }
    }

    /// Releases a local allocation and returns an excess empty slab with its exact page count.
    ///
    /// # Safety
    ///
    /// The pointer must be a live allocation from this allocator, with its original layout.
    /// The serialized owner must release it exactly once. Remote producers use the static path.
    pub unsafe fn dealloc(&mut self, object: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        let class = SizeClass::from_layout(layout).expect("layout exceeds slab size");
        let cache = &mut self.caches[class.index()];
        // SAFETY: The caller identifies a live slot of the cache selected by its original layout.
        match unsafe { cache.dealloc_object(VirtAddr::from_mut_ptr_of(object.as_ptr())) } {
            Some(base) => SlabDeallocResult::FreeSlab {
                base,
                pages: cache.layout().slab_size / self.page_size,
            },
            None => SlabDeallocResult::Done,
        }
    }

    /// Returns a live allocation's immutable owner without borrowing an allocator.
    ///
    /// Invalid page sizes and unsupported layouts are rejected before memory access.
    /// The original geometry locates the header by masking rather than scanning for a marker.
    ///
    /// # Safety
    ///
    /// The pointer must identify a live allocation with the supplied original layout and hardware
    /// page size. Backing must remain live throughout this call. No incompatible reference to the
    /// header may overlap the read of its immutable owner field.
    pub unsafe fn owner_cpu(
        object: NonNull<u8>,
        layout: Layout,
        page_size: usize,
    ) -> AllocResult<u16> {
        let page_log2 = Self::page_log2(page_size)?;
        let class = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let geometry = SlabLayout::for_class(class, page_log2);
        let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
        // SAFETY: The caller supplies a live allocation with matching backing geometry.
        let slab = unsafe { SlabPage::from_object(address, geometry) };
        // SAFETY: Only the immutable owner field is read while backing remains live.
        Ok(unsafe { (*slab.as_ptr().cast::<SlabPageHeader>()).owner_cpu })
    }

    /// Publishes a remote release after finding geometry through the shared read-only table.
    ///
    /// Invalid page sizes and unsupported layouts are rejected before memory access.
    /// This path never borrows the owner's allocator or accesses cache list heads.
    ///
    /// # Safety
    ///
    /// The pointer must be a live slot allocated with the supplied original layout and page size.
    /// It must be freed once. Backing must remain live until successful publication. The owner
    /// may then reuse or reclaim it immediately. No whole-header reference may overlap publication.
    pub unsafe fn remote_free_object(
        object: NonNull<u8>,
        layout: Layout,
        page_size: usize,
    ) -> AllocResult {
        let page_log2 = Self::page_log2(page_size)?;
        let class = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let geometry = SlabLayout::for_class(class, page_log2);
        let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
        // SAFETY: The caller guarantees a live slot with matching backing geometry.
        let slab = unsafe { SlabPage::from_object(address, geometry) };
        // SAFETY: The caller transfers one live slot to its sole owner through publication.
        unsafe { SlabPageHeader::remote_free(slab.cast(), address) };
        Ok(())
    }
}

impl Default for SlabAllocator {
    /// Creates an empty allocator configured for 4 KiB hardware pages.
    fn default() -> Self {
        Self::new(4096).expect("4 KiB hardware pages are supported")
    }
}
