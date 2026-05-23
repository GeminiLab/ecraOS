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
    page_meta::{PFN_NONE, PageFlags, free_list_push, free_list_remove},
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
    page_size: usize,
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
            page_size: 0,
            virt_phys_offset: 0,
            sections_head: ptr::null_mut(),
            sections_tail: ptr::null_mut(),
            section_count: 0,
        }
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
    pub unsafe fn init(
        &mut self,
        region: PhysAddrRange,
        page_size: usize,
        virt_phys_offset: usize,
    ) -> BuddyResult {
        unsafe {
            self.page_size = page_size;
            self.virt_phys_offset = virt_phys_offset;
            self.reset();
            self.add_region(region)
        }
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
        debug_assert!(self.page_size.is_power_of_two());

        // Normalize the region to the page size.
        let region = normalize_region(region, self.page_size).ok_or(BuddyError::InvalidParam)?;
        let region_virt = phys_range_to_virt_range(region, self.virt_phys_offset);
        let total_pages = region.size() / self.page_size;

        // Get the layout for the new section.
        let layout = BuddySection::layout_for_section(total_pages, self.page_size)
            .ok_or(BuddyError::InvalidParam)?;

        // Check for overlap with existing sections.
        for section in self.section_iter() {
            if section.region.overlaps(region_virt) {
                return Err(BuddyError::MemoryOverlap);
            }
        }

        let section = region_virt.start.as_mut_ptr_of();

        unsafe { BuddySection::init_at(section, region_virt, layout, self.page_size) };

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
        let page_size = self.page_size;

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

        for section in self.section_iter_mut() {
            if let Ok(vaddr) = Self::alloc_from_section_aligned(section, order, align, page_size) {
                return Ok(self.virt_to_phys(vaddr));
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Allocates a single physical frame.
    ///
    /// This is a convenience wrapper around [`alloc_frames`](Self::alloc_frames).
    pub fn alloc_frame(&mut self) -> BuddyResult<PhysAddr> {
        self.alloc_frames(1, self.page_size)
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
        unsafe {
            if count == 0 {
                return Err(BuddyError::InvalidParam);
            }

            let vaddr = self.phys_to_virt(paddr);
            let page_size = self.page_size;
            let section = self
                .find_section_by_addr_mut(vaddr)
                .ok_or(BuddyError::NotFound)?;

            let start_pfn = (vaddr - section.heap_region.start) / page_size;

            explat::dbcn_println!("alloc_frames_at: vaddr={:#x}, start_pfn={}, count={}", vaddr, start_pfn, count);

            if start_pfn
                .checked_add(count)
                .ok_or(BuddyError::InvalidParam)?
                > section.stats.heap_pages
            {
                return Err(BuddyError::InvalidParam);
            }

            let visitor = section.visitor();
            // Check that all target pages are free.
            for pfn in start_pfn..start_pfn + count {
                explat::dbcn_println!("  checking pfn {}: flags={:?}, order={}", pfn, visitor.meta[pfn].flags, visitor.meta[pfn].order);
                let m = &visitor.meta[pfn];
                if m.flags != PageFlags::Free {
                    return Err(BuddyError::NoMemory);
                }
            }

            let visitor = section.visitor_mut();
            // For each target PFN, find its containing free block and split down
            // to order 0.
            for target_pfn in start_pfn..start_pfn + count {
                let m = &visitor.meta[target_pfn];
                let current_order = m.order as usize;

                // Only the head PFN of a free block should be processed.
                // If this PFN is not the head, find the head.
                let block_pfn = target_pfn & !((1usize << current_order) - 1);
                let head_pfn = block_pfn as u32;

                // Remove the block from its free list.
                free_list_remove(visitor.meta, visitor.free_lists, head_pfn, current_order);

                // Split the block down to order 0, freeing the halves that
                // do not contain the target PFN.
                let mut cur_order = current_order;
                let mut cur_pfn = block_pfn;
                while cur_order > 0 {
                    cur_order -= 1;
                    let left_pfn = cur_pfn;
                    let right_pfn = cur_pfn + (1 << cur_order);
                    let (next_pfn, free_pfn) = if target_pfn >= right_pfn {
                        (right_pfn, left_pfn)
                    } else {
                        (left_pfn, right_pfn)
                    };
                    let fm = &mut visitor.meta[free_pfn];
                    fm.flags = PageFlags::Free;
                    fm.order = cur_order as u8;
                    free_list_push(visitor.meta, visitor.free_lists, free_pfn as u32, cur_order);
                    cur_pfn = next_pfn;
                }

                // Mark the target page as allocated.
                let tm = &mut visitor.meta[target_pfn];
                tm.flags = PageFlags::Allocated;
                tm.order = 0;
                visitor.stats.free_pages -= 1;
            }

            Ok(paddr)
        }
    }

    /// Frees physical frames previously obtained via [`alloc_frames`](Self::alloc_frames)
    /// or [`alloc_frame`](Self::alloc_frame).
    ///
    /// The allocator frees the full block size recorded in page metadata,
    /// which may be larger than `count` if the original allocation was rounded
    /// up for buddy order or alignment.
    pub fn dealloc_frames(&mut self, addr: PhysAddr, count: usize) {
        let vaddr = self.phys_to_virt(addr);
        let page_size = self.page_size;
        let Some(section) = self.find_section_by_addr_mut(vaddr) else {
            debug_assert!(
                false,
                "dealloc_frames called with address outside all sections"
            );
            return;
        };

        debug_assert!(vaddr.is_aligned(page_size));
        debug_assert!(count > 0);

        let pfn = (vaddr - section.heap_region.start) / page_size;
        debug_assert!(pfn < section.stats.heap_pages);
        let visitor = section.visitor();
        let stored = &visitor.meta[pfn];
        debug_assert!(
            stored.flags == PageFlags::Allocated,
            "dealloc_frames called on non-allocated block"
        );

        let expected_order = count.next_power_of_two().trailing_zeros() as usize;
        let order = stored.order as usize;
        debug_assert!(
            expected_order <= order,
            "dealloc_frames count implies larger order than the allocated block"
        );
        Self::dealloc_in_section(section, pfn, order);
    }

    /// Frees a single physical frame.
    ///
    /// This is a convenience wrapper around [`dealloc_frames`](Self::dealloc_frames).
    pub fn dealloc_frame(&mut self, addr: PhysAddr) {
        self.dealloc_frames(addr, 1);
    }

    /// Marks the page at the given physical address with the specified flags.
    ///
    /// This is typically used by the slab allocator to tag pages.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `addr` is a valid, properly allocated
    /// physical address within a managed region.
    pub unsafe fn set_page_flags(&mut self, addr: PhysAddr, flags: PageFlags) -> BuddyResult {
        let vaddr = self.phys_to_virt(addr);
        let page_size = self.page_size;
        let section = self
            .find_section_by_addr_mut(vaddr)
            .ok_or(BuddyError::NotFound)?;
        let pfn = (vaddr - section.heap_region.start) / page_size;
        let visitor = section.visitor_mut();
        visitor.meta[pfn].flags = flags;
        Ok(())
    }

    /// Returns the flags of the page containing the given physical address.
    pub fn page_flags(&self, addr: PhysAddr) -> BuddyResult<PageFlags> {
        let vaddr = self.phys_to_virt(addr);
        let section = self
            .find_section_by_addr(vaddr)
            .ok_or(BuddyError::NotFound)?;
        let pfn = (vaddr - section.heap_region.start) / self.page_size;
        let visitor = section.visitor();
        Ok(visitor.meta[pfn].flags)
    }

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

        let Some(region) = normalize_region(region, page_size) else {
            return false;
        };

        let total_pages = region.size() / page_size;

        let Some(layout) = BuddySection::layout_for_section(total_pages, page_size) else {
            return false;
        };

        let metadata_region = PhysAddrRange::new(
            region.start,
            region.start + layout.metadata_pages * page_size,
        );

        metadata_region.overlaps(check_range)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Attempts to allocate a block of the given order with alignment from a
    /// single section.
    ///
    /// Searches free lists from `order` up to `MAX_ORDER`, looking for a block
    /// that can satisfy the alignment requirement. When found, the block is
    /// split down to the target order.
    fn alloc_from_section_aligned(
        section: &mut BuddySection,
        order: usize,
        align: usize,
        page_size: usize,
    ) -> BuddyResult<VirtAddr> {
        let heap_start = section.heap_region.start;
        let visitor = section.visitor_mut();
        for search_order in order..=MAX_ORDER {
            let mut pfn_u32 = visitor.free_lists[search_order];
            while pfn_u32 != PFN_NONE {
                let block_pfn = pfn_u32 as usize;
                if let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    heap_start,
                    block_pfn,
                    search_order,
                    order,
                    align,
                    page_size,
                ) {
                    unsafe {
                        free_list_remove(visitor.meta, visitor.free_lists, pfn_u32, search_order);
                    }

                    let mut current_order = search_order;
                    let mut current_pfn = block_pfn;
                    while current_order > order {
                        current_order -= 1;
                        let left_pfn = current_pfn;
                        let right_pfn = current_pfn + (1 << current_order);
                        let (next_pfn, free_pfn) = if target_pfn >= right_pfn {
                            (right_pfn, left_pfn)
                        } else {
                            (left_pfn, right_pfn)
                        };
                        unsafe {
                            let bm = &mut visitor.meta[free_pfn];
                            bm.flags = PageFlags::Free;
                            bm.order = current_order as u8;
                            free_list_push(
                                visitor.meta,
                                visitor.free_lists,
                                free_pfn as u32,
                                current_order,
                            );
                        }
                        current_pfn = next_pfn;
                    }

                    let m = &mut visitor.meta[current_pfn];
                    m.flags = PageFlags::Allocated;
                    m.order = order as u8;

                    visitor.stats.free_pages -= 1 << order;
                    return Ok(section.heap_region.start + current_pfn * page_size);
                }
                pfn_u32 = visitor.meta[pfn_u32 as usize].next;
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Finds a PFN within a free block that satisfies the alignment requirement.
    ///
    /// Returns `None` if no suitably aligned sub-block exists within the block.
    fn find_aligned_pfn_in_block(
        heap_start: VirtAddr,
        block_pfn: usize,
        block_order: usize,
        alloc_order: usize,
        align: usize,
        page_size: usize,
    ) -> Option<usize> {
        let subblock_pages = 1usize << alloc_order;
        let align_pages = align / page_size;
        let heap_page_offset = (heap_start.as_usize() / page_size) & (align_pages - 1);
        let offset = (align_pages - heap_page_offset) & (align_pages - 1);

        let candidate = if align_pages <= subblock_pages {
            if !heap_start.is_aligned(align) {
                return None;
            }
            block_pfn
        } else {
            if !offset.is_multiple_of(subblock_pages) {
                return None;
            }
            let rem = block_pfn & (align_pages - 1);
            let delta = (offset + align_pages - rem) & (align_pages - 1);
            block_pfn + delta
        };

        let block_pages = 1usize << block_order;
        let last_start = block_pfn + block_pages - subblock_pages;
        (candidate <= last_start).then_some(candidate)
    }

    /// Performs buddy-merging deallocation within a single section.
    ///
    /// Starting from the given PFN and order, checks whether the buddy block
    /// is free and of the same order. If so, removes the buddy from its free
    /// list and merges into a block of the next order. Repeats until no more
    /// merging is possible, then pushes the resulting block onto its free list.
    fn dealloc_in_section(section: &mut BuddySection, mut pfn: usize, mut order: usize) {
        let visitor = section.visitor_mut();
        let freed_pages = 1usize << order;

        while order < MAX_ORDER {
            let buddy_pfn = pfn ^ (1 << order);
            if buddy_pfn >= visitor.stats.heap_pages {
                break;
            }
            let buddy = &visitor.meta[buddy_pfn];
            if buddy.flags != PageFlags::Free || buddy.order as usize != order {
                break;
            }
            unsafe {
                free_list_remove(visitor.meta, visitor.free_lists, buddy_pfn as u32, order);
            }
            pfn = pfn.min(buddy_pfn);
            order += 1;
        }

        unsafe {
            let m = &mut visitor.meta[pfn];
            m.flags = PageFlags::Free;
            m.order = order as u8;
            free_list_push(visitor.meta, visitor.free_lists, pfn as u32, order);
        }
        visitor.stats.free_pages += freed_pages;
    }

    /// Finds the section whose heap contains the given virtual address.
    fn find_section_by_addr(&self, addr: VirtAddr) -> Option<&BuddySection> {
        self.section_iter()
            .find(|section| section.contains_heap_addr(addr))
    }

    /// Finds the section whose heap contains the given virtual address (mutable).
    fn find_section_by_addr_mut(&mut self, addr: VirtAddr) -> Option<&mut BuddySection> {
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
