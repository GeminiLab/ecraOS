//! Buddy page allocator.
//!
//! Provides a page-level physical memory allocator using the buddy system
//! algorithm. Metadata and per-section state are stored in a prefix of each
//! managed memory region, requiring no dynamic allocation.

use core::ptr;

use memory_addr::{
    AddrRange, MemoryAddr, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange, pa, va,
};

use crate::{
    MAX_ORDER,
    error::{BuddyError, BuddyResult},
    pfn::PhysFrameNumber,
    section::BuddySection,
    stats::AllocatorStats,
};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Aligns and clamps a memory region to the given page size.
fn normalize_region<A: MemoryAddr>(region: AddrRange<A>, page_size: usize) -> Option<AddrRange<A>> {
    let start = region.start.align_up(page_size);
    let end = region.end.align_down(page_size);
    if end <= start {
        None
    } else {
        Some(AddrRange::new(start, end))
    }
}

/// Converts a physical address to a virtual address.
#[inline]
const fn phys_to_virt(paddr: PhysAddr, virt_phys_offset: usize) -> VirtAddr {
    va!(paddr.as_usize() + virt_phys_offset)
}

/// Converts a virtual address to a physical address.
#[inline]
const fn virt_to_phys(vaddr: VirtAddr, virt_phys_offset: usize) -> PhysAddr {
    pa!(vaddr.as_usize() - virt_phys_offset)
}

/// Converts a physical address range to a virtual address range.
#[inline]
fn phys_range_to_virt_range(range: PhysAddrRange, virt_phys_offset: usize) -> VirtAddrRange {
    VirtAddrRange::new(
        phys_to_virt(range.start, virt_phys_offset),
        phys_to_virt(range.end, virt_phys_offset),
    )
}

/// Converts a virtual address range to a physical address range.
#[inline]
#[expect(unused)]
fn virt_range_to_phys_range(range: VirtAddrRange, virt_phys_offset: usize) -> PhysAddrRange {
    PhysAddrRange::new(
        virt_to_phys(range.start, virt_phys_offset),
        virt_to_phys(range.end, virt_phys_offset),
    )
}

// ---------------------------------------------------------------------------
// BuddyAllocator
// ---------------------------------------------------------------------------

/// Buddy allocator for physical memory pages with configurable page size and
/// virtual-physical address offset.
///
/// Requires a fixed-offset mapping (like direct mapping area in Linux) for
/// conversions between physical and virtual addresses.
///
/// # In-memory layout
///
/// The allocator itself is a rather small struct, with only global configs
/// (page size and virtual-physical address offset) and pointers to "sections".
///
/// A section is a contiguous block of physical memory pages that is managed by
/// the allocator. It contains metadata pages (containing a header of type
/// [`BuddySection`], and a page metadata array of type [`PageMeta`]), and heap
/// pages (pages available for allocation). The memory layout of a section is as
/// follows:
///
/// ```text
/// ------------------------------   < aligned to page_size
///  Section header:
///    `struct BuddySection`
/// ------------------------------   < aligned to align_of<PageMeta>
///  Page metadata array:
///    `[PageMeta]` * N
/// ------------------------------   < aligned to page_size
///  Heap: N free pages
/// ------------------------------   < aligned to page_size
/// ```
///
/// Sections are linked together as a linked list.
pub struct BuddyAllocator {
    /// Page size in bytes (must be a power of two).
    page_size_shift: usize,
    /// Offset added to a physical address to obtain the corresponding virtual address.
    virt_phys_offset: usize,
    /// Head of the linked list of managed sections.
    ///
    /// `BuddyAllocator` itself is **NOT** a node of the linked list.
    sections_head: *mut BuddySection,
    /// Tail of the linked list of managed sections.
    ///
    /// `BuddyAllocator` itself is **NOT** a node of the linked list.
    sections_tail: *mut BuddySection,
    /// Number of managed sections.
    section_count: usize,
}

// SAFETY: The allocator is designed to be wrapped in a SpinMutex.
// All section pointers point into caller-provided regions whose lifetime
// is managed externally.
unsafe impl Send for BuddyAllocator {}

impl BuddyAllocator {
    /// Creates an uninitialised allocator.
    ///
    /// Call [`init`](Self::init) before use.
    pub const fn new() -> Self {
        Self {
            page_size_shift: 0,
            virt_phys_offset: 0,
            sections_head: ptr::null_mut(),
            sections_tail: ptr::null_mut(),
            section_count: 0,
        }
    }

    #[inline]
    const fn page_size(&self) -> usize {
        1usize << self.page_size_shift
    }

    #[inline]
    const fn phys_to_virt(&self, addr: PhysAddr) -> VirtAddr {
        phys_to_virt(addr, self.virt_phys_offset)
    }

    #[inline]
    const fn virt_to_phys(&self, addr: VirtAddr) -> PhysAddr {
        virt_to_phys(addr, self.virt_phys_offset)
    }

    pub fn section_iter(&self) -> BuddySectionIter<'_> {
        BuddySectionIter {
            _allocator: self,
            current: self.sections_head,
        }
    }

    pub fn section_iter_mut(&mut self) -> BuddySectionIterMut<'_> {
        let head = self.sections_head;
        BuddySectionIterMut {
            _allocator: self,
            current: head,
        }
    }
}

impl Default for BuddyAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl BuddyAllocator {
    /// Resets the allocator to the uninitialised state.
    fn reset(&mut self) {
        self.sections_head = ptr::null_mut();
        self.sections_tail = ptr::null_mut();
        self.section_count = 0;
    }

    /// Initializes the allocator over the first memory region.
    ///
    /// The region is given as a physical address range. It is converted to
    /// virtual addresses internally using `virt_phys_offset`. Metadata is
    /// stored in the region prefix.
    ///
    /// # Safety
    ///
    /// - The physical memory region must be writable (mapped) and remain valid
    ///   for the lifetime of this allocator.
    pub unsafe fn init(&mut self, page_size: usize, virt_phys_offset: usize) -> BuddyResult {
        debug_assert!(page_size.is_power_of_two());
        self.page_size_shift = page_size.trailing_zeros() as usize;
        self.virt_phys_offset = virt_phys_offset;
        self.reset();
        Ok(())
    }

    /// Adds a new managed memory region after initialization.
    ///
    /// The region is given as a physical address range and is converted to
    /// virtual addresses internally.
    ///
    /// # Safety
    ///
    /// - The physical memory region must be writable (mapped) and remain valid
    ///   for the lifetime of this allocator.
    /// - The region must not overlap any existing managed region.
    pub unsafe fn add_region(&mut self, region: PhysAddrRange) -> BuddyResult {
        let page_size = self.page_size();
        let page_size_shift = self.page_size_shift;

        // Normalize the region to the page size.
        let region = normalize_region(region, page_size).ok_or(BuddyError::InvalidParam)?;
        let region_virt = phys_range_to_virt_range(region, self.virt_phys_offset);
        let total_pages = region.size() >> page_size_shift;

        // Get the layout for the new section.
        let layout = BuddySection::layout_for_section(total_pages, page_size_shift)
            .ok_or(BuddyError::InvalidParam)?;

        // Check for overlap with existing sections.
        for section in self.section_iter() {
            if section.region.overlaps(region_virt) {
                return Err(BuddyError::MemoryOverlap);
            }
        }

        let section = region_virt.start.as_mut_ptr_of();
        let heap_start = region.start + (layout.metadata_pages << page_size_shift);
        let heap_start_pfn = PhysFrameNumber::from_phys_addr(heap_start, page_size_shift);

        unsafe {
            BuddySection::init_at(
                section,
                region_virt,
                layout,
                heap_start_pfn,
                page_size_shift,
            )
        };

        if self.sections_head.is_null() {
            self.sections_head = section;
        } else {
            // SAFETY: When `self.section_head` is not null, `self.section_tail`
            // is also not null because we know at least one section has been
            // added.
            unsafe {
                self.sections_tail.as_mut_unchecked().next = section;
            }
        }
        self.sections_tail = section;
        self.section_count += 1;

        Ok(())
    }

    /// Returns the number of managed sections.
    pub fn section_count(&self) -> usize {
        self.section_count
    }

    /// Returns a read-only summary for a managed section by registration order.
    pub fn section_stats(&self, index: usize) -> Option<AllocatorStats> {
        self.section_iter().nth(index).map(BuddySection::stats)
    }

    /// Returns a read-only summary for all managed sections.
    pub fn stats(&self) -> AllocatorStats {
        self.section_iter().map(BuddySection::stats).sum()
    }

    /// Allocates `count` contiguous physical frames with the given alignment.
    ///
    /// Returns the starting physical address of the allocation on success.
    pub fn alloc_frames(&mut self, count: usize, align: usize) -> BuddyResult<PhysAddr> {
        let page_size = self.page_size();

        if count == 0 {
            return Err(BuddyError::InvalidParam);
        }

        if !align.is_power_of_two() || align < page_size {
            return Err(BuddyError::InvalidAlignment);
        }

        let order = count.next_power_of_two().trailing_zeros() as usize;
        if order > MAX_ORDER {
            return Err(BuddyError::NoMemory);
        }

        let page_size_shift = self.page_size_shift;
        for section in self.section_iter_mut() {
            if let Ok(vaddr) = section.alloc_frames(order, align, page_size_shift) {
                return Ok(self.virt_to_phys(vaddr));
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Allocates a single physical frame.
    ///
    /// This is a convenience wrapper around [`alloc_frames`](Self::alloc_frames).
    pub fn alloc_frame(&mut self) -> BuddyResult<PhysAddr> {
        self.alloc_frames(1, self.page_size())
    }

    /// Allocates `count` contiguous physical frames starting at the given physical address.
    ///
    /// All target pages must be free. Each target page is obtained by finding
    /// its containing free block and splitting it down to order 0.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `paddr` and `paddr + count * page_size`
    /// are valid physical addresses within a managed region, and that no other
    /// references to those frames exist.
    pub unsafe fn alloc_frames_at(
        &mut self,
        paddr: PhysAddr,
        count: usize,
    ) -> BuddyResult<PhysAddr> {
        if count == 0 {
            return Err(BuddyError::InvalidParam);
        }

        let vaddr = self.phys_to_virt(paddr);
        let page_size_shift = self.page_size_shift;
        let section = self
            .find_section_by_addr_mut(vaddr)
            .ok_or(BuddyError::NotFound)?;
        section.alloc_frames_at(vaddr, count, page_size_shift)?;
        Ok(paddr)
    }

    /// Frees physical frames previously obtained via [`alloc_frames`](Self::alloc_frames)
    /// or [`alloc_frame`](Self::alloc_frame).
    ///
    /// The allocator frees the full block size recorded in page metadata,
    /// which may be larger than `count` if the original allocation was rounded
    /// up for buddy order or alignment.
    pub fn dealloc_frames(&mut self, addr: PhysAddr, count: usize) {
        let vaddr = self.phys_to_virt(addr);
        let page_size_shift = self.page_size_shift;
        let Some(section) = self.find_section_by_addr_mut(vaddr) else {
            debug_assert!(
                false,
                "dealloc_frames called with address outside all sections"
            );
            return;
        };

        section.dealloc_frames(vaddr, count, page_size_shift);
    }

    /// Frees a single physical frame.
    ///
    /// This is a convenience wrapper around [`dealloc_frames`](Self::dealloc_frames).
    pub fn dealloc_frame(&mut self, addr: PhysAddr) {
        self.dealloc_frames(addr, 1);
    }

    // /// Marks the page at the given physical address with the specified flags.
    // ///
    // /// This is typically used by the slab allocator to tag pages.
    // ///
    // /// # Safety
    // ///
    // /// The caller must ensure that `addr` is a valid, properly allocated
    // /// physical address within a managed region.
    // pub unsafe fn set_page_flags(&mut self, addr: PhysAddr, flags: PageFlags) -> BuddyResult {
    //     let vaddr = self.phys_to_virt(addr);
    //     let page_size = self.page_size();
    //     let section = self
    //         .find_section_by_addr_mut(vaddr)
    //         .ok_or(BuddyError::NotFound)?;
    //     section.set_page_flags(vaddr, flags, page_size)
    // }

    // /// Returns the flags of the page containing the given physical address.
    // pub fn page_flags(&self, addr: PhysAddr) -> BuddyResult<PageFlags> {
    //     let vaddr = self.phys_to_virt(addr);
    //     let section = self
    //         .find_section_by_addr(vaddr)
    //         .ok_or(BuddyError::NotFound)?;
    //     section.page_flags(vaddr, self.page_size())
    // }

    /// Checks whether the intrusive buddy metadata for a region would overlap
    /// the given check range.
    ///
    /// Returns `true` if adding `region` with the given `page_size` would place
    /// metadata in `[section_start, managed_heap_start)` that overlaps
    /// `check_range`.
    ///
    /// This does NOT modify the allocator state.
    pub fn check_metadata_overlap(
        region: PhysAddrRange,
        page_size: usize,
        check_range: PhysAddrRange,
    ) -> bool {
        debug_assert!(page_size.is_power_of_two());
        let page_size_shift = page_size.trailing_zeros() as usize;

        let Some(region) = normalize_region(region, page_size) else {
            return false;
        };

        let total_pages = region.size() >> page_size_shift;

        let Some(layout) = BuddySection::layout_for_section(total_pages, page_size_shift) else {
            return false;
        };

        let metadata_region = PhysAddrRange::new(
            region.start,
            region.start + (layout.metadata_pages << page_size_shift),
        );

        metadata_region.overlaps(check_range)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Finds the section whose heap contains the given virtual address.
    pub fn find_section_by_addr(&self, addr: VirtAddr) -> Option<&BuddySection> {
        self.section_iter()
            .find(|section| section.contains_heap_addr(addr))
    }

    /// Finds the section whose heap contains the given virtual address (mutable).
    pub fn find_section_by_addr_mut(&mut self, addr: VirtAddr) -> Option<&mut BuddySection> {
        self.section_iter_mut()
            .find(|section| section.contains_heap_addr(addr))
    }
}

pub struct BuddySectionIter<'a> {
    /// The allocator that owns the sections.
    ///
    /// This is used to ensure that the allocator is valid for the lifetime of the iterator.
    _allocator: &'a BuddyAllocator,
    /// The current section in the iterator.
    current: *const BuddySection,
}

impl<'a> Iterator for BuddySectionIter<'a> {
    type Item = &'a BuddySection;

    fn next(&mut self) -> Option<Self::Item> {
        let current = unsafe { self.current.as_ref()? };
        self.current = current.next;
        Some(current)
    }
}

pub struct BuddySectionIterMut<'a> {
    /// The allocator that owns the sections.
    ///
    /// This is used to ensure that the allocator is valid for the lifetime of the iterator.
    _allocator: &'a mut BuddyAllocator,
    /// The current section in the iterator.
    current: *mut BuddySection,
}

impl<'a> Iterator for BuddySectionIterMut<'a> {
    type Item = &'a mut BuddySection;

    fn next(&mut self) -> Option<Self::Item> {
        let current = unsafe { self.current.as_mut()? };
        self.current = current.next;
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{vec, vec::Vec};

    use memory_addr::{MemoryAddr, pa};

    use crate::pfn::SectionFrameNumber;

    use super::*;

    fn page_aligned_buffer(page_size: usize, pages: usize) -> (*mut u8, Vec<u8>) {
        let mut buffer = vec![0; pages * page_size + page_size];
        let start = buffer.as_mut_ptr() as usize;
        let aligned = start.align_up(page_size) as *mut u8;
        (aligned, buffer)
    }

    #[test]
    fn alloc_frames_handles_section_heap_not_aligned_to_requested_alignment() {
        let page_size = 4096;
        let total_pages = 64;
        let (region_ptr, _buffer) = page_aligned_buffer(page_size, total_pages + 8);
        let region_paddr = pa!(region_ptr as usize + page_size * 5);
        let virt_phys_offset = 0;
        let region = PhysAddrRange::new(region_paddr, region_paddr + total_pages * page_size);

        let mut allocator = BuddyAllocator::new();
        unsafe {
            allocator.init(page_size, virt_phys_offset).unwrap();
            allocator.add_region(region).unwrap();
        }

        let allocation = allocator.alloc_frames(4, page_size * 8).unwrap();

        assert_eq!(allocation.as_usize() & (page_size * 8 - 1), 0);
    }

    #[test]
    fn dealloc_frames_merges_only_absolute_physical_buddies() {
        let page_size = 4096;
        let total_pages = 64;
        let (region_ptr, _buffer) = page_aligned_buffer(page_size, total_pages + 8);
        let region_paddr = pa!(region_ptr as usize + page_size * 5);
        let virt_phys_offset = 0;
        let region = PhysAddrRange::new(region_paddr, region_paddr + total_pages * page_size);

        let mut allocator = BuddyAllocator::new();
        unsafe {
            allocator.init(page_size, virt_phys_offset).unwrap();
            allocator.add_region(region).unwrap();
        }

        let section = allocator.section_iter_mut().next().unwrap();
        let heap_start = section.heap_region.start;
        let page0 = SectionFrameNumber::new(0).to_addr(heap_start, 12);
        let page1 = SectionFrameNumber::new(1).to_addr(heap_start, 12);

        section.alloc_frames_at(page0, 1, 12).unwrap();
        section.alloc_frames_at(page1, 1, 12).unwrap();
        section.dealloc_frames(page0, 1, 12);
        section.dealloc_frames(page1, 1, 12);

        let allocation = section.alloc_frames(1, page_size * 2, 12).unwrap();

        assert_ne!(allocation, page0);
        assert_eq!(allocation.as_usize() & (page_size * 2 - 1), 0);
    }
}
