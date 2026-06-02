//! Buddy page allocator.
//!
//! Provides a page-level physical memory allocator using the buddy system
//! algorithm.

use core::{hint::unlikely, ptr};

use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, va};

use crate::{
    error::{BuddyError, BuddyResult},
    page_count_to_order_ceiling,
    pfn::SectionFrameNumber,
    section::{BuddySection, SectionLayout},
    stats::AllocatorStats,
};

/// Buddy allocator for physical memory pages with configurable page size and
/// virtual-physical address offset.
///
/// Requires a fixed-offset mapping (like direct mapping area in Linux) for
/// conversions between physical and virtual addresses.
///
/// # In-memory layout
///
/// The allocator itself is a relatively small struct, containing only global configs
/// (page size and virtual-physical address offset) and pointers to "sections".
/// Sections are linked together as a linked list.
///
/// A section is a contiguous range of physical memory pages that is managed by
/// the allocator. It consists of the metadata pages (containing a header of type
/// [`BuddySection`], and an array of heap page metadata of type [`PageMeta`]), and then the heap
/// pages (pages available for allocation). The memory layout of a section is as
/// follows:
///
/// ```text
/// +------------------------------+   < aligned to page_size
/// | Section header               |
/// |     `struct BuddySection`    |
/// +------------------------------+   < aligned to align_of::<PageMeta>()
/// | Page metadata array          |
/// |     `[PageMeta]` * N         |
/// +------------------------------+   < aligned to page_size
/// | Heap                         |
/// |     N free pages             |
/// +------------------------------+   < aligned to page_size
/// ```
///
/// On very rare occasions, there may be an unused page at the end of the section.
///
/// ## Buddy blocks
///
/// Like any other buddy allocator, the allocator uses buddy blocks to organize the heap pages.
///
/// In this buddy allocator, blocks' buddies are determined by the absolute physical frame number,
/// not by the relative, section-local frame number. For example, for a heap located at physical
/// address 0x1000 ~ 0x9000, with page size 0x1000, the initial state of the allocator is:
///
/// ```text
/// +------------------------------+   < 0x1000
/// | Free block, order 0          |
/// +------------------------------+   < 0x2000
/// | Free block, order 1          |
/// +------------------------------+   < 0x4000
/// | Free block, order 2          |
/// +------------------------------+   < 0x8000
/// | Free block, order 0          |
/// +------------------------------+   < 0x9000
/// ```
///
/// Such arrangement is useful as it ensures that all blocks are naturally aligned to its maximal
/// possible order (and therefore all possible orders).
///
pub struct BuddyAllocator {
    /// Page size shift.
    ///
    /// Must be greater than 0. 0 is reserved for uninitialized state.
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

impl Default for BuddyAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Creation and section management/iteration.
impl BuddyAllocator {
    /// Creates an uninitialized allocator.
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

    /// Initializes the allocator with the given page size and virtual-physical
    /// address offset.
    ///
    /// The page size must be larger than 1 (page size shift must be greater
    /// than 0).
    ///
    /// # Safety
    ///
    /// - The `virt_phys_offset` must be valid.
    pub unsafe fn init(&mut self, page_size_shift: usize, virt_phys_offset: usize) -> BuddyResult {
        if page_size_shift == 0 {
            return Err(BuddyError::InvalidPageSize);
        }

        self.page_size_shift = page_size_shift;
        self.virt_phys_offset = virt_phys_offset;
        self.sections_head = ptr::null_mut();
        self.sections_tail = ptr::null_mut();
        self.section_count = 0;

        Ok(())
    }

    /// Returns whether the allocator is initialized.
    #[inline]
    pub fn is_initialized(&self) -> bool {
        self.page_size_shift > 0
    }

    /// Ensures that the allocator is initialized.
    ///
    /// Returns `Err(BuddyError::NotInitialized)` if the allocator is not
    /// initialized.
    #[inline]
    pub fn ensure_initialized(&self) -> BuddyResult {
        if unlikely(!self.is_initialized()) {
            return Err(BuddyError::NotInitialized);
        }

        Ok(())
    }

    /// Adds a new managed memory section from a physical memory region.
    ///
    /// # Safety
    ///
    /// - The physical memory region must be valid and writable for the lifetime of this allocator.
    pub unsafe fn add_section(&mut self, mem_region: PhysAddrRange) -> BuddyResult {
        self.ensure_initialized()?;

        // Get the layout for the new section.
        let layout = self.layout_for_section_on(mem_region)?;

        if layout.heap_pages > Self::MAX_HEAP_PAGES_IN_SECTION {
            return Err(BuddyError::SectionTooLarge);
        }

        // Check for overlap with existing sections.
        for section in self.section_iter() {
            if section.region.overlaps(mem_region) {
                return Err(BuddyError::SectionOverlap);
            }
        }

        // Get the pointer to the new section.
        let start_virt = va!(mem_region.start.as_usize() + self.virt_phys_offset);
        let section = start_virt.as_mut_ptr_of();

        // SAFETY: the validity of the `section` and `region_virt` is guaranteed
        // by the caller.
        unsafe { BuddySection::init_at(section, mem_region, layout, self.page_size_shift) };

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

    /// Checks whether the metadata region of the section that would be created
    /// by adding the given physical memory region would overlap the given check
    /// range.
    ///
    /// Returns `Err(BuddyError)` if the memory region is invalid to be added as
    /// a section.
    pub fn check_metadata_overlap(
        &self,
        mem_region: PhysAddrRange,
        check_range: PhysAddrRange,
    ) -> BuddyResult<bool> {
        let layout = self.layout_for_section_on(mem_region)?;

        let metadata_region = PhysAddrRange::new(
            mem_region.start,
            mem_region.start + (layout.metadata_pages << self.page_size_shift),
        );

        Ok(metadata_region.overlaps(check_range))
    }

    /// Returns the number of managed sections.
    pub fn section_count(&self) -> usize {
        self.section_count
    }

    /// Returns an [immutable iterator](BuddySectionIter) over the managed
    /// sections.
    pub fn section_iter(&self) -> BuddySectionIter<'_> {
        BuddySectionIter {
            _allocator: self,
            current: self.sections_head,
        }
    }

    /// Returns a [mutable iterator](BuddySectionIterMut) over the managed
    /// sections.
    pub fn section_iter_mut(&mut self) -> BuddySectionIterMut<'_> {
        let head = self.sections_head;
        BuddySectionIterMut {
            _allocator: self,
            current: head,
        }
    }

    /// Finds the section whose heap contains the given virtual address.
    pub fn find_section_by_addr(&self, addr: PhysAddr) -> Option<&BuddySection> {
        self.section_iter()
            .find(|section| section.contains_heap_addr(addr))
    }

    /// Finds the section whose heap contains the given virtual address and
    /// returns a mutable reference to it.
    pub fn find_section_by_addr_mut(&mut self, addr: PhysAddr) -> Option<&mut BuddySection> {
        self.section_iter_mut()
            .find(|section| section.contains_heap_addr(addr))
    }

    /// Returns a read-only summary for a managed section by registration order.
    pub fn section_stats(&self, index: usize) -> Option<AllocatorStats> {
        self.section_iter().nth(index).map(BuddySection::stats)
    }

    /// Returns a read-only summary for all managed sections.
    pub fn stats(&self) -> AllocatorStats {
        self.section_iter().map(BuddySection::stats).sum()
    }
}

/// Utility functions.
impl BuddyAllocator {
    /// Returns the layout for the section that would be created by adding the
    /// given physical memory region.
    ///
    /// The memory region must be aligned to the page size, and must be large
    /// enough to contain at least one heap page.
    #[inline]
    fn layout_for_section_on(&self, mem_region: PhysAddrRange) -> BuddyResult<SectionLayout> {
        let page_size_shift = self.page_size_shift;
        let page_size = self.page_size();

        if !mem_region.start.is_aligned(page_size) || !mem_region.end.is_aligned(page_size) {
            return Err(BuddyError::NotAligned);
        }

        BuddySection::layout_for_section(mem_region.size() >> page_size_shift, page_size_shift)
            .ok_or(BuddyError::SectionTooSmall)
    }

    /// Returns the page size for the allocator.
    #[inline]
    const fn page_size(&self) -> usize {
        1usize << self.page_size_shift
    }

    /// The maximal number of heap pages in a section.
    pub const MAX_HEAP_PAGES_IN_SECTION: usize = SectionFrameNumber::MAX_VALID_SFN.as_usize() + 1;
}

/// Primitive allocation and deallocation.
impl BuddyAllocator {
    /// Allocates a single buddy block.
    ///
    /// The unit of `align` is bytes. It must be a power of two and be no less
    /// than the page size.
    pub fn alloc_block(&mut self, order: usize, align: usize) -> BuddyResult<PhysAddr> {
        let page_size_shift = self.page_size_shift;
        for section in self.section_iter_mut() {
            match section.alloc_block(order, align, page_size_shift) {
                Err(BuddyError::NoMemory) => continue,
                result => return result,
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Deallocates a single buddy block.
    ///
    /// The `order` must be the same as the order of the block that was
    /// allocated.
    pub fn dealloc_block(&mut self, addr: PhysAddr, order: usize) -> BuddyResult {
        let page_size_shift = self.page_size_shift;
        let section = self
            .find_section_by_addr_mut(addr)
            .ok_or(BuddyError::NotInHeap)?;
        section.dealloc_block(addr, order, page_size_shift)
    }

    /// Allocates multiple contiguous buddy blocks starting at the given
    /// physical address and having the given total page count.
    ///
    /// The `addr` must be aligned to the page size, or this function will return an error.
    ///
    /// The specified address range must be free, or this function will return an error.
    pub fn alloc_blocks_at(&mut self, addr: PhysAddr, page_count: usize) -> BuddyResult {
        let page_size_shift = self.page_size_shift;
        let section = self
            .find_section_by_addr_mut(addr)
            .ok_or(BuddyError::NotInHeap)?;
        section.alloc_blocks_at(addr, page_count, page_size_shift)
    }

    /// Deallocates multiple contiguous buddy blocks starting at the given
    /// physical address and having the given total page count.
    ///
    /// The `addr` must be aligned to the page size, or this function will return an error.
    ///
    /// The specified address range must be allocated, and must match the block border of the
    /// allocated blocks, or this function will return an error.
    pub fn dealloc_blocks_at(&mut self, addr: PhysAddr, page_count: usize) -> BuddyResult {
        let page_size_shift = self.page_size_shift;
        let section = self
            .find_section_by_addr_mut(addr)
            .ok_or(BuddyError::NotInHeap)?;
        section.dealloc_blocks_at(addr, page_count, page_size_shift)
    }
}

/// Derived allocation and deallocation.
impl BuddyAllocator {
    /// Allocates `count` contiguous physical frames with the given alignment.
    ///
    /// This function is a convenience wrapper around [`alloc_block`](Self::alloc_block), and always
    /// allocates a full buddy block.
    pub fn alloc_frames(&mut self, count: usize, align: usize) -> BuddyResult<PhysAddr> {
        self.alloc_block(page_count_to_order_ceiling(count)?, align)
    }

    /// Allocates a single physical frame.
    ///
    /// This is a convenience wrapper around [`alloc_frames`](Self::alloc_frames).
    pub fn alloc_frame(&mut self) -> BuddyResult<PhysAddr> {
        self.alloc_frames(1, self.page_size())
    }

    /// Frees physical frames previously obtained via [`alloc_frames`](Self::alloc_frames).
    ///
    /// This function is a convenience wrapper around [`dealloc_block`](Self::dealloc_block), and
    /// expects the `count` to be the same as the one used when calling
    /// [`alloc_frames`](Self::alloc_frames). Mismatched count **MAY** result in error.
    pub fn dealloc_frames(&mut self, addr: PhysAddr, count: usize) -> BuddyResult {
        self.dealloc_block(addr, page_count_to_order_ceiling(count)?)
    }

    /// Frees a single physical frame.
    ///
    /// This is a convenience wrapper around [`dealloc_frames`](Self::dealloc_frames).
    pub fn dealloc_frame(&mut self, addr: PhysAddr) -> BuddyResult<()> {
        self.dealloc_frames(addr, 1)
    }

    /// Allocates `count` contiguous physical frames starting at the given physical address.
    ///
    /// This function is merely a wrapper around [`alloc_blocks_at`](Self::alloc_blocks_at).
    pub fn alloc_frames_at(&mut self, addr: PhysAddr, count: usize) -> BuddyResult {
        self.alloc_blocks_at(addr, count)
    }

    /// Frees `count` contiguous physical frames starting at the given physical address.
    ///
    /// This function is merely a wrapper around [`dealloc_blocks_at`](Self::dealloc_blocks_at).
    /// `addr` and `count` are expected to match the ones that were used when calling
    /// [`alloc_frames_at`](Self::alloc_frames_at). Mismatched `addr` and `count` **MAY** result in
    /// error.
    pub fn dealloc_frames_at(&mut self, addr: PhysAddr, count: usize) -> BuddyResult {
        self.dealloc_blocks_at(addr, count)
    }

    /// Returns whether the page containing the physical address is allocated.
    ///
    /// Unlike other functions, this function does not require the `addr` to be aligned to the page
    /// size.
    pub fn is_allocated(&self, addr: PhysAddr) -> BuddyResult<bool> {
        let section = self
            .find_section_by_addr(addr)
            .ok_or(BuddyError::NotInHeap)?;
        section.is_allocated(addr, self.page_size_shift)
    }
}

/// An immutable iterator over the [`BuddySection`]s managed by a [`BuddyAllocator`].
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
        // SAFETY: We DO know that `self.current` is either null (when the iterator is exhausted)
        // or a valid pointer to a `BuddySection` (the caller to `BuddyAllocator::add_section`
        // promised it).
        let current = unsafe { self.current.as_ref()? };
        self.current = current.next;
        Some(current)
    }
}

/// A mutable iterator over the [`BuddySection`]s managed by a [`BuddyAllocator`].
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
        // SAFETY: We DO know that `self.current` is either null (when the iterator is exhausted)
        // or a valid pointer to a `BuddySection` (the caller to `BuddyAllocator::add_section`
        // promised it).
        let current = unsafe { self.current.as_mut()? };
        self.current = current.next;
        Some(current)
    }
}
