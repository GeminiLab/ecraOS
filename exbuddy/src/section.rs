use core::{mem, ptr, slice};

use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up};

use crate::{
    MAX_ORDER,
    error::{BuddyError, BuddyResult},
    page_meta::{PFN_NONE, PageFlags, PageMeta, free_list_push, free_list_remove},
    stats::AllocatorStats,
};

pub struct BuddySectionLinkedListVisitor<'a> {
    pub meta: &'a [PageMeta],
    pub free_lists: &'a [u32; MAX_ORDER + 1],
    pub stats: &'a AllocatorStats,
}

pub struct BuddySectionLinkedListVisitorMut<'a> {
    pub meta: &'a mut [PageMeta],
    pub free_lists: &'a mut [u32; MAX_ORDER + 1],
    pub stats: &'a mut AllocatorStats,
}

pub struct SectionLayout {
    pub total_pages: usize,
    pub metadata_pages: usize,
    pub heap_pages: usize,
}

impl SectionLayout {
    pub fn heap_region(&self, base_addr: VirtAddr, page_size: usize) -> VirtAddrRange {
        VirtAddrRange::new(
            base_addr + self.metadata_pages * page_size,
            base_addr + (self.metadata_pages + self.heap_pages) * page_size,
        )
    }
}

/// Per-region buddy state stored in the region prefix.
///
/// All addresses stored in this struct are virtual addresses (raw `usize`).
#[repr(C)]
pub struct BuddySection {
    /// Pointer to the next section in the linked list.
    pub next: *mut BuddySection,
    /// Full range of the section in virtual addresses, including the header,
    /// the page metadata array, and the heap.
    pub region: VirtAddrRange,
    /// Range of the allocatable heap.
    ///
    /// On very rare cases, `heap_region.end` may not be equal to `region.end`.
    pub heap_region: VirtAddrRange,
    /// Usage statistics for the allocator.
    pub stats: AllocatorStats,
    /// Per-order free-list heads (PFN indices).
    pub free_lists: [u32; MAX_ORDER + 1],
}

// Asserts that the section header is aligned to the page meta alignment, which
// means, no padding bytes are needed between the section header and the
// metadata array.
const _: () = assert!(mem::size_of::<BuddySection>().is_multiple_of(mem::align_of::<PageMeta>()));

impl BuddySection {
    /// Returns the size of the metadata region (the header and the page
    /// metadata array) in bytes with a given number of heap pages.
    pub const fn metadata_size(heap_pages: usize) -> usize {
        mem::size_of::<BuddySection>() + heap_pages * mem::size_of::<PageMeta>()
    }

    /// Guesses the number of pages that can be allocated to the heap with a
    /// given total number of pages and page size.
    const fn heap_pages_guess(total_pages: usize, page_size: usize) -> usize {
        debug_assert!(
            page_size.is_power_of_two(),
            "page_size must be a power of two"
        );

        if total_pages <= 1 {
            return 0;
        }

        let total_bytes = total_pages * page_size;
        (total_bytes - mem::size_of::<BuddySection>()) / (page_size + mem::size_of::<PageMeta>())
    }

    /// Computes the optimal layout for a section with a given total number of
    /// pages and page size, or `None` if no heap pages can be allocated.
    pub const fn layout_for_section(total_pages: usize, page_size: usize) -> Option<SectionLayout> {
        debug_assert!(
            page_size.is_power_of_two(),
            "page_size must be a power of two"
        );

        let mut heap_pages = Self::heap_pages_guess(total_pages, page_size);
        while heap_pages > 0 {
            let metadata_pages = align_up(Self::metadata_size(heap_pages), page_size) / page_size;

            if metadata_pages + heap_pages <= total_pages {
                return Some(SectionLayout {
                    metadata_pages,
                    heap_pages,
                    total_pages,
                });
            }

            heap_pages -= 1;
        }

        None
    }

    /// Returns the maximum buddy order that can be allocated starting at a
    /// given page frame number (PFN) without exceeding the maximum order.
    pub const fn max_order_for_pfn(pfn: usize) -> usize {
        let order = pfn.trailing_zeros() as usize;
        if order > MAX_ORDER { MAX_ORDER } else { order }
    }

    /// Returns the maximum buddy order that can be allocated starting at a
    /// given page frame number (PFN) within the heap without exceeding
    /// the maximum order.
    pub const fn max_order_for_pfn_in_heap(pfn: usize, heap_pages: usize) -> usize {
        let mut order = Self::max_order_for_pfn(pfn);
        while order > 0 {
            if pfn + (1usize << order) <= heap_pages {
                break;
            }
            order -= 1;
        }
        order
    }

    pub unsafe fn init_at(
        ptr: *mut BuddySection,
        region: VirtAddrRange,
        layout: SectionLayout,
        page_size: usize,
    ) {
        unsafe {
            ptr.write(BuddySection {
                next: ptr::null_mut(),
                region,
                heap_region: layout.heap_region(region.start, page_size),
                free_lists: [PFN_NONE; MAX_ORDER + 1],
                stats: AllocatorStats {
                    total_pages: layout.total_pages,
                    meta_pages: layout.metadata_pages,
                    heap_pages: layout.heap_pages,
                    free_pages: 0,
                },
            })
        };

        let section = unsafe { ptr.as_mut_unchecked() };
        let visitor = section.visitor_mut();
        let heap_pages = layout.heap_pages;
        let mut pfn = 0;
        while pfn < heap_pages {
            let order = Self::max_order_for_pfn_in_heap(pfn, heap_pages);
            let block_pages = 1usize << order;

            let m = &mut visitor.meta[pfn];
            m.flags = PageFlags::Free;
            m.order = order as u8;
            unsafe { free_list_push(visitor.meta, visitor.free_lists, pfn as u32, order) };

            visitor.stats.free_pages += block_pages;
            pfn += block_pages;
        }
    }

    pub fn visitor(&self) -> BuddySectionLinkedListVisitor<'_> {
        let meta = {
            let self_ptr = self as *const _ as *const u8;
            let meta_ptr = unsafe { self_ptr.byte_add(mem::size_of::<Self>()) as *const PageMeta };
            unsafe { slice::from_raw_parts(meta_ptr, self.stats.heap_pages) }
        };
        let free_lists = &self.free_lists;
        let stats = &self.stats;
        BuddySectionLinkedListVisitor {
            meta,
            free_lists,
            stats,
        }
    }

    pub fn visitor_mut(&mut self) -> BuddySectionLinkedListVisitorMut<'_> {
        let meta = {
            let self_ptr = self as *mut _ as *mut u8;
            let meta_ptr = unsafe { self_ptr.byte_add(mem::size_of::<Self>()) as *mut PageMeta };
            unsafe { slice::from_raw_parts_mut(meta_ptr, self.stats.heap_pages) }
        };
        let free_lists = &mut self.free_lists;
        let stats = &mut self.stats;
        BuddySectionLinkedListVisitorMut {
            meta,
            free_lists,
            stats,
        }
    }

    /// Allocates frames from this section.
    ///
    /// Searches this section's free lists for a block of `order` that satisfies
    /// `align`, splits larger blocks as needed, and returns the allocated virtual
    /// address.
    pub fn alloc_frames(
        &mut self,
        order: usize,
        align: usize,
        page_size: usize,
    ) -> BuddyResult<VirtAddr> {
        let heap_start = self.heap_region.start;
        let visitor = self.visitor_mut();
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
                        free_list_remove(visitor.meta, visitor.free_lists, pfn_u32, search_order)
                    };

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
                        let bm = &mut visitor.meta[free_pfn];
                        bm.flags = PageFlags::Free;
                        bm.order = current_order as u8;
                        unsafe {
                            free_list_push(
                                visitor.meta,
                                visitor.free_lists,
                                free_pfn as u32,
                                current_order,
                            )
                        };
                        current_pfn = next_pfn;
                    }

                    let m = &mut visitor.meta[current_pfn];
                    m.flags = PageFlags::Allocated;
                    m.order = order as u8;

                    visitor.stats.free_pages -= 1 << order;
                    return Ok(self.heap_region.start + current_pfn * page_size);
                }
                pfn_u32 = visitor.meta[pfn_u32 as usize].next;
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Allocates frames starting at a specific virtual address in this section.
    ///
    /// Finds the free block containing each target page, splits it to order 0,
    /// and marks the requested pages allocated.
    pub fn alloc_frames_at(
        &mut self,
        addr: VirtAddr,
        count: usize,
        page_size: usize,
    ) -> BuddyResult<VirtAddr> {
        let start_pfn = self.checked_pfn_range_start(addr, count, page_size)?;

        let visitor = self.visitor();
        for pfn in start_pfn..start_pfn + count {
            if Self::find_free_block_containing(visitor.free_lists, visitor.meta, pfn).is_none() {
                return Err(BuddyError::NoMemory);
            }
        }

        let visitor = self.visitor_mut();
        for target_pfn in start_pfn..start_pfn + count {
            let (block_pfn, current_order) =
                Self::find_free_block_containing(visitor.free_lists, visitor.meta, target_pfn)
                    .ok_or(BuddyError::NoMemory)?;
            let head_pfn = block_pfn as u32;

            unsafe { free_list_remove(visitor.meta, visitor.free_lists, head_pfn, current_order) };

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
                unsafe {
                    free_list_push(visitor.meta, visitor.free_lists, free_pfn as u32, cur_order)
                };
                cur_pfn = next_pfn;
            }

            let tm = &mut visitor.meta[target_pfn];
            tm.flags = PageFlags::Allocated;
            tm.order = 0;
            visitor.stats.free_pages -= 1;
        }

        Ok(addr)
    }

    /// Deallocates frames in this section.
    ///
    /// Reads the stored allocation order from the head page and merges free
    /// buddy blocks before returning the resulting block to the free list.
    pub fn dealloc_frames(&mut self, addr: VirtAddr, count: usize, page_size: usize) {
        debug_assert!(addr.is_aligned(page_size));
        debug_assert!(count > 0);

        let pfn = (addr - self.heap_region.start) / page_size;
        debug_assert!(pfn < self.stats.heap_pages);
        let visitor = self.visitor();
        let stored = &visitor.meta[pfn];
        debug_assert!(
            stored.flags == PageFlags::Allocated,
            "dealloc_frames called on non-allocated block"
        );

        let expected_order = count.next_power_of_two().trailing_zeros() as usize;
        let mut order = stored.order as usize;
        debug_assert!(
            expected_order <= order,
            "dealloc_frames count implies larger order than the allocated block"
        );

        let visitor = self.visitor_mut();
        let freed_pages = 1usize << order;
        let mut pfn = pfn;

        while order < MAX_ORDER {
            let buddy_pfn = pfn ^ (1 << order);
            if buddy_pfn >= visitor.stats.heap_pages {
                break;
            }
            let buddy = &visitor.meta[buddy_pfn];
            if buddy.flags != PageFlags::Free || buddy.order as usize != order {
                break;
            }
            unsafe { free_list_remove(visitor.meta, visitor.free_lists, buddy_pfn as u32, order) };
            pfn = pfn.min(buddy_pfn);
            order += 1;
        }

        let m = &mut visitor.meta[pfn];
        m.flags = PageFlags::Free;
        m.order = order as u8;
        unsafe { free_list_push(visitor.meta, visitor.free_lists, pfn as u32, order) };
        visitor.stats.free_pages += freed_pages;
    }

    /// Marks the page containing the virtual address with the specified flags.
    pub fn set_page_flags(
        &mut self,
        addr: VirtAddr,
        flags: PageFlags,
        page_size: usize,
    ) -> BuddyResult {
        let pfn = self.checked_pfn(addr, page_size)?;
        let visitor = self.visitor_mut();
        visitor.meta[pfn].flags = flags;
        Ok(())
    }

    // /// Returns the flags of the page containing the virtual address.
    // pub fn page_flags(&self, addr: VirtAddr, page_size: usize) -> BuddyResult<PageFlags> {
    //     let pfn = self.checked_pfn(addr, page_size)?;
    //     let visitor = self.visitor();
    //     Ok(visitor.meta[pfn].flags)
    // }

    /// Checks if the given virtual address falls within this section's heap.
    #[inline]
    pub fn contains_heap_addr(&self, addr: VirtAddr) -> bool {
        self.heap_region.contains(addr)
    }

    /// Returns a read-only summary of this section.
    #[inline]
    pub fn stats(&self) -> AllocatorStats {
        self.stats.clone()
    }

    /// Returns the section-local PFN for the given virtual address.
    fn checked_pfn(&self, addr: VirtAddr, page_size: usize) -> BuddyResult<usize> {
        if !self.heap_region.contains(addr) || !addr.is_aligned(page_size) {
            return Err(BuddyError::InvalidParam);
        }
        Ok((addr - self.heap_region.start) / page_size)
    }

    /// Returns the start PFN for an in-section virtual address range.
    fn checked_pfn_range_start(
        &self,
        addr: VirtAddr,
        count: usize,
        page_size: usize,
    ) -> BuddyResult<usize> {
        let start_pfn = self.checked_pfn(addr, page_size)?;
        if start_pfn
            .checked_add(count)
            .ok_or(BuddyError::InvalidParam)?
            > self.stats.heap_pages
        {
            return Err(BuddyError::InvalidParam);
        }
        Ok(start_pfn)
    }

    /// Finds the free block containing the given PFN.
    ///
    /// Searches free-list heads because only free block heads have reliable
    /// metadata after allocations and merges.
    fn find_free_block_containing(
        free_lists: &[u32; MAX_ORDER + 1],
        meta: &[PageMeta],
        pfn: usize,
    ) -> Option<(usize, usize)> {
        for (order, free_list) in free_lists.iter().enumerate() {
            let block_pages = 1usize << order;
            let mut head = *free_list;
            while head != PFN_NONE {
                let block_pfn = head as usize;
                if pfn >= block_pfn && pfn < block_pfn + block_pages {
                    return Some((block_pfn, order));
                }
                head = meta[block_pfn].next;
            }
        }
        None
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
        let block_pages = 1usize << block_order;
        let last_start = block_pfn + block_pages - subblock_pages;
        let mut candidate = block_pfn;

        while candidate <= last_start {
            if (heap_start + candidate * page_size).is_aligned(align) {
                return Some(candidate);
            }
            candidate += subblock_pages;
        }

        None
    }
}
