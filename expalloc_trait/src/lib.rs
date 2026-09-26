//! Abstract page allocator interface.
//!
//! This crate defines [`PageAllocator`], a static interface used by low-level
//! components that need physically addressed pages and virtual addresses to
//! them, such as page-table implementations. The accompanying
//! [`DynPageAllocator`] function-pointer table permits choosing
//! an implementation at runtime.

#![no_std]

use memory_addr::{PhysAddr, VirtAddr};

/// A physical page allocator.
///
/// Implementations allocate physically contiguous, page-aligned frames and
/// translate their physical addresses into virtual addresses through which the
/// caller can access them. All frame counts use the page size reported by
/// [`Self::page_size_shift`].
#[dyn_static_traits::dyn_static_traits(DynPageAllocator)]
pub trait PageAllocator {
    /// Returns the base-2 logarithm of the allocator's page size.
    ///
    /// A page contains `1 << page_size_shift()` bytes. The value must remain
    /// constant for the lifetime of every allocation made through this type.
    fn page_size_shift() -> usize;

    /// Allocates one frame.
    ///
    /// By default, this delegates to [`Self::alloc_frames`] with a page count
    /// of one.
    fn alloc_frame() -> Option<PhysAddr> {
        Self::alloc_frames(1)
    }

    /// Allocates a contiguous run of frames.
    ///
    /// Returns the physical address of the first frame, or [`None`] when the
    /// requested run cannot be allocated. The returned address must be aligned
    /// to the allocator's page size.
    fn alloc_frames(page_count: usize) -> Option<PhysAddr>;

    /// Allocates enough contiguous frames to cover a byte size.
    ///
    /// By default, this delegates to [`Self::alloc_frames`] with a page count
    /// computed from the byte size.
    fn alloc_frames_of_size(size_in_bytes: usize) -> Option<PhysAddr> {
        Self::alloc_frames(size_in_bytes.div_ceil(1usize << Self::page_size_shift()))
    }

    /// Deallocates one frame.
    ///
    /// By default, this delegates to [`Self::dealloc_frames`] with a page count
    /// of one.
    fn dealloc_frame(addr: PhysAddr) {
        Self::dealloc_frames(addr, 1)
    }

    /// Deallocates a contiguous run of frames.
    ///
    /// `addr` and `page_count` must exactly match a previous allocation
    /// returned by this allocator.
    fn dealloc_frames(addr: PhysAddr, page_count: usize);

    /// Deallocates the frames covering a byte size.
    ///
    /// By default, this delegates to [`Self::dealloc_frames`] with a page count
    /// computed from the byte size. Therefore, `size_in_bytes` could actually
    /// be different from the actual number used when allocating frames. However,
    /// this is not promised to be the case for every allocator.
    fn dealloc_frames_of_size(addr: PhysAddr, size_in_bytes: usize) {
        Self::dealloc_frames(
            addr,
            size_in_bytes.div_ceil(1usize << Self::page_size_shift()),
        )
    }

    /// Translates a physical address into an accessible virtual address.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
}
