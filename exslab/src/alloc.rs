//! The slab allocator.
//!
//! [`SlabAllocator`] owns one cache per size class and selects the class from an allocation layout.
//! It allocates no memory itself. When a class needs a slab, the allocator reports how many pages
//! to supply, and the caller registers that memory and retries. The caller also serializes local
//! operations and decides how to route a free, using [`SlabAllocator::owner_cpu`] to find the
//! owning CPU. Remote frees reach a slab header without borrowing the allocator.
//!
//! [`SlabCache`] offers the same machinery for a single caller-selected class.

use core::{alloc::Layout, ptr::NonNull};

use memory_addr::VirtAddr;

use crate::{
    SlabLayout,
    cache::SlabCache,
    error::{AllocError, AllocResult},
    size_class::SizeClass,
    slab_page::{SlabPage, SlabPageHeader},
};

/// Result of a slab allocation attempt.
pub enum SlabAllocResult {
    /// A successfully allocated object.
    Allocated(NonNull<u8>),
    /// Slab memory that the caller must supply before retrying the allocation.
    ///
    /// The caller provides exactly this many contiguous mapped writable pages, aligned to the whole
    /// span, and registers them with [`SlabAllocator::add_slab`].
    NeedsSlab {
        /// Size class whose cache needs a slab.
        size_class: SizeClass,
        /// Exact number of hardware pages in the required slab.
        ///
        /// The caller must supply exactly this many contiguous mapped writable pages, aligned to
        /// the total size of the pages.
        pages: usize,
    },
}

/// Result of a local slab deallocation.
pub enum SlabDeallocResult {
    /// The object was released and its slab stays cached.
    Done,
    /// The slab became empty and exceeded the cache's retained empty slab, so the caller must
    /// release its memory.
    FreeSlab {
        /// Virtual base address of the complete slab memory region.
        base: VirtAddr,
        /// Exact number of hardware pages to release.
        pages: usize,
    },
}

/// A slab allocator over caller-supplied slab memory.
///
/// The hardware page size is fixed at construction to 4, 16, or 64 KiB. Slab sizes follow
/// `max(min(65536, slot_size * 512), page_size)` and must be aligned to their complete span, where
/// `slot_size` comes from the selected size class. See the [crate documentation](crate) for the
/// page counts this produces.
///
/// The caller serializes operations through the allocator and keeps slab memory alive until it is
/// returned or every user of it has finished. Dropping the allocator does not release slab memory.
pub struct SlabAllocator {
    /// Independent class states, each permanently bound to a static layout.
    caches: [SlabCache; SizeClass::SIZE_CLASS_COUNT],
    /// Hardware page size used to report slab memory requests and returns.
    page_size: usize,
}

/// Allocation, slab registration, and ownership operations for [`SlabAllocator`].
impl SlabAllocator {
    /// Creates an empty slab allocator.
    ///
    /// `page_size` must be 4, 16, or 64 KiB, and other values return [`AllocError::InvalidParam`].
    /// No slab memory is obtained here. Each class begins empty and asks for a slab on first use.
    pub fn new(page_size: usize) -> AllocResult<Self> {
        let page_log2 = Self::page_log2(page_size)?;
        Ok(Self {
            caches: core::array::from_fn(|index| {
                SlabCache::with_layout(SlabLayout::for_class(SizeClass::ALL[index], page_log2))
            }),
            page_size,
        })
    }

    /// Returns the base-2 logarithm of a supported hardware page size in bytes.
    fn page_log2(page_size: usize) -> AllocResult<usize> {
        match page_size {
            4096 => Ok(12),
            16384 => Ok(14),
            65536 => Ok(16),
            _ => Err(AllocError::InvalidParam),
        }
    }

    /// Allocates an object matching the requested allocation layout.
    ///
    /// Selects the size class from the layout and takes a slot from that class. Returns
    /// [`SlabAllocResult::NeedsSlab`] when the class has no slot available. A size or alignment
    /// above 2048 bytes returns [`AllocError::InvalidParam`].
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

    /// Registers slab memory for the selected size class.
    ///
    /// The memory must be the size that [`Self::alloc`] reported for `class`, and the allocator
    /// takes no separate size argument because the class fixes it. A zero or misaligned base
    /// returns [`AllocError::InvalidParam`] before memory is accessed.
    ///
    /// Registration is unsafe, so safe code cannot register an arbitrary address as slab memory.
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
    /// An accepted base must identify exclusive mapped writable slab memory of the exact requested
    /// span, aligned to that span. Keep this memory valid until it is returned, or until all cache
    /// operations, slots, and remote producers end.
    pub unsafe fn add_slab(
        &mut self,
        class: SizeClass,
        base: VirtAddr,
        owner_cpu: u16,
    ) -> AllocResult {
        // SAFETY: The caller supplies complete exclusive slab memory for this cache's bound layout.
        unsafe { self.caches[class.index()].add_slab(base, owner_cpu) }
    }

    /// Releases an object through its owning allocator.
    ///
    /// Each size class retains at most one empty slab. When another slab of that class becomes
    /// empty, it is removed and returned through [`SlabDeallocResult::FreeSlab`] with its exact
    /// page count.
    ///
    /// # Safety
    ///
    /// The pointer must be a live allocation from this allocator, with its original allocation
    /// layout. Release it exactly once and serialize this call with the allocator's other
    /// operations. Remote frees use [`Self::remote_free_object`].
    pub unsafe fn dealloc(&mut self, object: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        let class = SizeClass::from_layout(layout).expect("unsupported allocation layout");
        let cache = &mut self.caches[class.index()];
        // SAFETY: The caller identifies a live slot of the cache selected by its allocation layout.
        match unsafe { cache.dealloc_object(VirtAddr::from_mut_ptr_of(object.as_ptr())) } {
            Some(base) => SlabDeallocResult::FreeSlab {
                base,
                pages: cache.layout().slab_size / self.page_size,
            },
            None => SlabDeallocResult::Done,
        }
    }

    /// Returns the logical CPU identifier that owns an allocation.
    ///
    /// Requires only the object address, its original allocation layout, and the hardware page
    /// size, so it works from any CPU and borrows no allocator state. A kernel typically
    /// compares the result with the current CPU to choose between a local and a remote free.
    /// The slab size for the layout determines which address bits to mask off, so no header is
    /// scanned to find the owner.
    ///
    /// An unsupported layout or page size returns [`AllocError::InvalidParam`] before memory is
    /// accessed.
    ///
    /// # Safety
    ///
    /// The pointer must identify a live allocation with its original allocation layout and hardware
    /// page size. Slab memory must remain valid throughout this call. No incompatible reference to
    /// the header may overlap the read of its immutable owner field.
    pub unsafe fn owner_cpu(
        object: NonNull<u8>,
        layout: Layout,
        page_size: usize,
    ) -> AllocResult<u16> {
        let page_log2 = Self::page_log2(page_size)?;
        let class = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let slab_layout = SlabLayout::for_class(class, page_log2);
        let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
        // SAFETY: The caller supplies a live allocation with a matching slab layout.
        let slab = unsafe { SlabPage::from_object(address, slab_layout) };
        // SAFETY: Only the immutable owner field is read while the slab memory remains valid.
        Ok(unsafe { (*slab.as_ptr().cast::<SlabPageHeader>()).owner_cpu })
    }

    /// Releases an object that belongs to another CPU's allocator.
    ///
    /// Publishes the slot to its slab through [`SlabPageHeader::remote_free`], which needs no
    /// access to the owning allocator. The owner drains published slots when a later allocation or
    /// deallocation uses that slab. This call does not return slab memory itself.
    ///
    /// An unsupported layout or page size returns [`AllocError::InvalidParam`] before memory is
    /// accessed.
    ///
    /// # Safety
    ///
    /// The pointer must be a live slot with its original allocation layout and hardware page size.
    /// It must be freed once. Slab memory must remain valid until successful publication. The owner
    /// may then reuse the slot or reclaim the slab memory immediately. No whole-header reference
    /// may overlap publication.
    pub unsafe fn remote_free_object(
        object: NonNull<u8>,
        layout: Layout,
        page_size: usize,
    ) -> AllocResult {
        let page_log2 = Self::page_log2(page_size)?;
        let class = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let slab_layout = SlabLayout::for_class(class, page_log2);
        let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
        // SAFETY: The caller guarantees a live slot with a matching slab layout.
        let slab = unsafe { SlabPage::from_object(address, slab_layout) };
        // SAFETY: The caller transfers one live slot to its sole owner through publication.
        unsafe { SlabPageHeader::remote_free(slab.cast(), address) };
        Ok(())
    }
}

/// Default hardware page configuration for [`SlabAllocator`].
impl Default for SlabAllocator {
    /// Creates an empty allocator configured for 4 KiB hardware pages.
    fn default() -> Self {
        Self::new(4096).expect("4 KiB hardware pages are supported")
    }
}
