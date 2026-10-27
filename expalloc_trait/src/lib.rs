#![doc = include_str!("../README.md")]
#![no_std]

use memory_addr::{PhysAddr, VirtAddr};

/// A physical page allocator with a static interface.
///
/// Implementations allocate physically contiguous, page-aligned frames and translate their
/// physical addresses into virtual addresses through which the caller can access them. All frame
/// counts use the page size reported by [`Self::page_size_shift`]. The associated functions have
/// no receiver, so generic code selects an implementation through its type parameter.
/// [`DynPageAllocator`] stores the same functions for runtime selection.
///
/// # Allocation and access requirements
///
/// Allocations must remain reserved until released and must not overlap other live allocations.
/// The interface does not guarantee that newly allocated memory is initialized. Callers must end
/// all accesses before releasing an allocation and must not release it more than once.
///
/// Initialization, mapping availability, and concurrency requirements depend on the implementation.
/// The default methods do not initialize the allocator, establish mappings, or synchronize access.
#[dyn_static_traits::dyn_static_traits(DynPageAllocator)]
pub trait PageAllocator {
    /// Returns the base-2 logarithm of the allocator's page size.
    ///
    /// A page contains `1usize << page_size_shift()` bytes. The result must be less than
    /// `usize::BITS` and must remain constant for the lifetime of every allocation made through
    /// this type.
    fn page_size_shift() -> usize;

    /// Allocates one physical frame.
    ///
    /// By default, this delegates to [`Self::alloc_frames`] with a page count of one and returns
    /// [`None`] when the allocation cannot be satisfied.
    fn alloc_frame() -> Option<PhysAddr> {
        Self::alloc_frames(1)
    }

    /// Allocates a contiguous run of physical frames.
    ///
    /// For a nonzero `page_count`, returns the physical address of the first frame, or [`None`]
    /// when the requested run cannot be allocated. The returned address must be aligned to the
    /// allocator's page size, and the allocation must cover `page_count` consecutive pages.
    /// Zero-count behavior is defined by the implementation.
    fn alloc_frames(page_count: usize) -> Option<PhysAddr>;

    /// Allocates enough contiguous frames to cover a byte size.
    ///
    /// By default, this rounds `size_in_bytes` up to a whole number of pages and delegates to
    /// [`Self::alloc_frames`]. A zero byte size delegates with a count of zero, so its behavior
    /// depends on the implementation. Implementations may override this method to track the
    /// original byte size. Callers should release the allocation through
    /// [`Self::dealloc_frames_of_size`] with the same byte size.
    fn alloc_frames_of_size(size_in_bytes: usize) -> Option<PhysAddr> {
        Self::alloc_frames(size_in_bytes.div_ceil(1usize << Self::page_size_shift()))
    }

    /// Deallocates one physical frame.
    ///
    /// `addr` must identify a live one-frame allocation managed by this allocator. By default,
    /// this delegates to [`Self::dealloc_frames`] with a page count of one.
    fn dealloc_frame(addr: PhysAddr) {
        Self::dealloc_frames(addr, 1)
    }

    /// Deallocates a contiguous run of physical frames.
    ///
    /// `addr` and `page_count` must exactly match the starting address and frame count of a live
    /// allocation managed by this allocator. All accesses must have ended before this call.
    /// Zero-count behavior is defined by the implementation.
    fn dealloc_frames(addr: PhysAddr, page_count: usize);

    /// Deallocates the frames covering a byte size.
    ///
    /// By default, this rounds `size_in_bytes` up to a whole number of pages and delegates to
    /// [`Self::dealloc_frames`]. A zero byte size delegates with a count of zero.
    ///
    /// `addr` must identify a live allocation managed by this allocator, and `size_in_bytes` should
    /// be the original byte size passed to [`Self::alloc_frames_of_size`]. Different byte sizes
    /// that round to the same page count work with the default implementation, but an override may
    /// require the exact original byte size. All accesses must have ended before this call.
    fn dealloc_frames_of_size(addr: PhysAddr, size_in_bytes: usize) {
        Self::dealloc_frames(
            addr,
            size_in_bytes.div_ceil(1usize << Self::page_size_shift()),
        )
    }

    /// Translates a physical address into an accessible virtual address.
    ///
    /// `addr` must be supported by the implementation's physical-memory mapping. The returned
    /// address must access the same physical memory, with a mapping that covers the bytes the
    /// caller needs to access. Callers must keep the mapping and any allocation being accessed
    /// valid for the duration of those accesses. Translation does not transfer ownership.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
}
