#![no_std]

use memory_addr::{PhysAddr, VirtAddr};

/// Generic page allocator trait.
#[dyn_static_traits::dyn_static_traits(DynPageAllocator)]
pub trait PageAllocator {
    /// Gets the page size shift, i.e. log2(page_size).
    fn page_size_shift() -> usize;

    /// Allocates a single frame.
    fn alloc_frame() -> Option<PhysAddr> {
        Self::alloc_frames(1)
    }

    /// Allocates multiple frames.
    fn alloc_frames(page_count: usize) -> Option<PhysAddr>;

    /// Allocates frames of a given size.
    fn alloc_frames_of_size(size_in_bytes: usize) -> Option<PhysAddr> {
        Self::alloc_frames(size_in_bytes.div_ceil(1usize << Self::page_size_shift()))
    }

    /// Deallocates a single frame.
    fn dealloc_frame(addr: PhysAddr) {
        Self::dealloc_frames(addr, 1)
    }

    /// Deallocates multiple frames.
    fn dealloc_frames(addr: PhysAddr, page_count: usize);

    /// Deallocates frames of a given size.
    fn dealloc_frames_of_size(addr: PhysAddr, size_in_bytes: usize) {
        Self::dealloc_frames(
            addr,
            size_in_bytes.div_ceil(1usize << Self::page_size_shift()),
        )
    }

    /// Converts a physical address to a virtual address.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
}
