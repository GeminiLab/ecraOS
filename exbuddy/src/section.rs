use core::{mem, ptr, slice};

use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up};

use crate::{
    MAX_ORDER,
    error::{BuddyError, BuddyResult},
    page_meta::{PFN_NONE, PageFlags, PageMeta, decode_pfn, free_list_push, free_list_remove},
    pfn::{PhysFrameNumber, SectionFrameNumber},
    stats::AllocatorStats,
};

pub struct BuddySectionLinkedListVisitor<'a> {
    pub meta: &'a [PageMeta],
    pub free_lists: &'a [SectionFrameNumber; MAX_ORDER + 1],
    pub stats: &'a AllocatorStats,
}

pub struct BuddySectionLinkedListVisitorMut<'a> {
    pub meta: &'a mut [PageMeta],
    pub free_lists: &'a mut [SectionFrameNumber; MAX_ORDER + 1],
    pub stats: &'a mut AllocatorStats,
}

pub struct SectionLayout {
    pub total_pages: usize,
    pub metadata_pages: usize,
    pub heap_pages: usize,
}

impl SectionLayout {
    pub fn heap_region(&self, base_addr: VirtAddr, page_size_shift: usize) -> VirtAddrRange {
        VirtAddrRange::new(
            base_addr + (self.metadata_pages << page_size_shift),
            base_addr + ((self.metadata_pages + self.heap_pages) << page_size_shift),
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
    /// Absolute physical PFN for section-local heap PFN 0.
    pub heap_start_pfn: PhysFrameNumber,
    /// Usage statistics for the allocator.
    pub stats: AllocatorStats,
    /// Per-order free-list heads (section-local PFN indices).
    pub free_lists: [SectionFrameNumber; MAX_ORDER + 1],
}

// Asserts that the section header is aligned to the page meta alignment, which
// means, no padding bytes are needed between the section header and the
// metadata array.
const _: () = assert!(mem::size_of::<BuddySection>().is_multiple_of(mem::align_of::<PageMeta>()));

impl BuddySection {
    /// Returns the size of the metadata region in bytes.
    ///
    /// Computes the size of the section header plus one page metadata entry for
    /// each heap page.
    pub const fn metadata_size(heap_pages: usize) -> usize {
        mem::size_of::<BuddySection>() + heap_pages * mem::size_of::<PageMeta>()
    }

    /// Guesses the number of pages that can be allocated to the heap.
    ///
    /// Estimates the heap capacity for a section with the given total page count
    /// and page size shift.
    const fn heap_pages_guess(total_pages: usize, page_size_shift: usize) -> usize {
        if total_pages <= 1 {
            return 0;
        }

        let page_size = 1usize << page_size_shift;
        let total_bytes = total_pages << page_size_shift;
        (total_bytes - mem::size_of::<BuddySection>()) / (page_size + mem::size_of::<PageMeta>())
    }

    /// Computes the optimal layout for a section.
    ///
    /// Returns `None` if the section cannot fit both metadata and at least one
    /// heap page.
    pub const fn layout_for_section(
        total_pages: usize,
        page_size_shift: usize,
    ) -> Option<SectionLayout> {
        let page_size = 1usize << page_size_shift;
        let mut heap_pages = Self::heap_pages_guess(total_pages, page_size_shift);
        while heap_pages > 0 {
            let metadata_pages =
                align_up(Self::metadata_size(heap_pages), page_size) >> page_size_shift;

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

    /// Returns the maximum buddy order that can be allocated at a physical PFN.
    ///
    /// Computes the largest order whose block-size alignment is satisfied by
    /// the absolute physical page frame number, capped by [`MAX_ORDER`].
    pub const fn max_order_for_abs_pfn(abs_pfn: PhysFrameNumber) -> usize {
        let order = abs_pfn.max_order();
        if order > MAX_ORDER { MAX_ORDER } else { order }
    }

    /// Returns the maximum buddy order that can be allocated within the heap.
    ///
    /// Computes the largest order for a section-local PFN by checking the
    /// absolute page-frame alignment and the remaining heap size.
    pub const fn max_order_for_pfn_in_heap(
        abs_pfn: PhysFrameNumber,
        pfn: SectionFrameNumber,
        heap_pages: usize,
    ) -> usize {
        let mut order = Self::max_order_for_abs_pfn(abs_pfn);
        while order > 0 {
            if pfn.as_usize() + (1usize << order) <= heap_pages {
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
        heap_start_pfn: PhysFrameNumber,
        page_size_shift: usize,
    ) {
        unsafe {
            ptr.write(BuddySection {
                next: ptr::null_mut(),
                region,
                heap_region: layout.heap_region(region.start, page_size_shift),
                heap_start_pfn,
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
        let mut pfn = SectionFrameNumber::new(0);
        while pfn.as_usize() < heap_pages {
            let abs_pfn = heap_start_pfn + pfn.as_usize();
            let order = Self::max_order_for_pfn_in_heap(abs_pfn, pfn, heap_pages);
            let block_pages = 1usize << order;

            let m = &mut visitor.meta[pfn.as_usize()];
            m.flags = PageFlags::Free;
            m.order = order as u8;
            unsafe { free_list_push(visitor.meta, visitor.free_lists, pfn, order) };

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
        page_size_shift: usize,
    ) -> BuddyResult<VirtAddr> {
        let heap_start_pfn = self.heap_start_pfn;
        let heap_start = self.heap_region.start;
        let visitor = self.visitor_mut();
        // Search from the requested order to the maximum order.
        for search_order in order..=MAX_ORDER {
            // Iterate over the free list for the current order.
            let mut block_pfn_opt = decode_pfn(visitor.free_lists[search_order]);
            while let Some(block_pfn) = block_pfn_opt {
                // Find suitable start PFN in the free block.
                if let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    heap_start_pfn,
                    block_pfn,
                    search_order,
                    order,
                    align,
                    page_size_shift,
                ) {
                    unsafe {
                        free_list_remove(visitor.meta, visitor.free_lists, block_pfn, search_order)
                    };

                    // Split the block into smaller blocks.
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
                        let bm = &mut visitor.meta[free_pfn.as_usize()];
                        bm.flags = PageFlags::Free;
                        bm.order = current_order as u8;
                        unsafe {
                            free_list_push(
                                visitor.meta,
                                visitor.free_lists,
                                free_pfn,
                                current_order,
                            )
                        };
                        current_pfn = next_pfn;
                    }

                    let m = &mut visitor.meta[current_pfn.as_usize()];
                    m.flags = PageFlags::Allocated;
                    m.order = order as u8;

                    visitor.stats.free_pages -= 1 << order;
                    return Ok(current_pfn.to_addr(heap_start, page_size_shift));
                }
                block_pfn_opt = decode_pfn(visitor.meta[block_pfn.as_usize()].next);
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
        page_size_shift: usize,
    ) -> BuddyResult<VirtAddr> {
        let start_pfn = self.checked_pfn_range_start(addr, count, page_size_shift)?;
        let end_pfn = start_pfn + count;

        let visitor = self.visitor();
        let mut pfn = start_pfn;
        while pfn < end_pfn {
            if Self::find_free_block_containing(visitor.free_lists, visitor.meta, pfn).is_none() {
                return Err(BuddyError::NoMemory);
            }
            pfn += 1;
        }

        let visitor = self.visitor_mut();
        let mut target_pfn = start_pfn;
        while target_pfn < end_pfn {
            let (block_pfn, current_order) =
                Self::find_free_block_containing(visitor.free_lists, visitor.meta, target_pfn)
                    .ok_or(BuddyError::NoMemory)?;

            unsafe { free_list_remove(visitor.meta, visitor.free_lists, block_pfn, current_order) };

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
                let fm = &mut visitor.meta[free_pfn.as_usize()];
                fm.flags = PageFlags::Free;
                fm.order = cur_order as u8;
                unsafe { free_list_push(visitor.meta, visitor.free_lists, free_pfn, cur_order) };
                cur_pfn = next_pfn;
            }

            let tm = &mut visitor.meta[target_pfn.as_usize()];
            tm.flags = PageFlags::Allocated;
            tm.order = 0;
            visitor.stats.free_pages -= 1;
            target_pfn += 1;
        }

        Ok(addr)
    }

    /// Deallocates frames in this section.
    ///
    /// Reads the stored allocation order from the head page and merges free
    /// buddy blocks before returning the resulting block to the free list.
    pub fn dealloc_frames(&mut self, addr: VirtAddr, count: usize, page_size_shift: usize) {
        let page_size = 1usize << page_size_shift;
        debug_assert!(addr.is_aligned(page_size));
        debug_assert!(count > 0);

        let pfn = self
            .checked_pfn(addr, page_size_shift)
            .expect("dealloc_frames called with address outside section heap");
        debug_assert!(pfn.as_usize() < self.stats.heap_pages);
        let visitor = self.visitor();
        let stored = &visitor.meta[pfn.as_usize()];
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

        let heap_start_pfn = self.heap_start_pfn;
        let visitor = self.visitor_mut();
        let freed_pages = 1usize << order;
        let mut pfn = pfn;

        while order < MAX_ORDER {
            let Some(buddy_pfn) = pfn.buddy(order, heap_start_pfn, visitor.stats.heap_pages) else {
                break;
            };
            let buddy = &visitor.meta[buddy_pfn.as_usize()];
            if buddy.flags != PageFlags::Free || buddy.order as usize != order {
                break;
            }
            unsafe { free_list_remove(visitor.meta, visitor.free_lists, buddy_pfn, order) };
            pfn = pfn.min(buddy_pfn);
            order += 1;
        }

        let m = &mut visitor.meta[pfn.as_usize()];
        m.flags = PageFlags::Free;
        m.order = order as u8;
        unsafe { free_list_push(visitor.meta, visitor.free_lists, pfn, order) };
        visitor.stats.free_pages += freed_pages;
    }

    /// Marks the page containing the virtual address with the specified flags.
    pub fn set_page_flags(
        &mut self,
        addr: VirtAddr,
        flags: PageFlags,
        page_size_shift: usize,
    ) -> BuddyResult {
        let pfn = self.checked_pfn(addr, page_size_shift)?;
        let visitor = self.visitor_mut();
        visitor.meta[pfn.as_usize()].flags = flags;
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
    fn checked_pfn(
        &self,
        addr: VirtAddr,
        page_size_shift: usize,
    ) -> BuddyResult<SectionFrameNumber> {
        let page_size = 1usize << page_size_shift;
        if !self.heap_region.contains(addr) || !addr.is_aligned(page_size) {
            return Err(BuddyError::InvalidParam);
        }
        Ok(SectionFrameNumber::new(
            (addr - self.heap_region.start) >> page_size_shift,
        ))
    }

    /// Returns the start PFN for an in-section virtual address range.
    fn checked_pfn_range_start(
        &self,
        addr: VirtAddr,
        count: usize,
        page_size_shift: usize,
    ) -> BuddyResult<SectionFrameNumber> {
        let start_pfn = self.checked_pfn(addr, page_size_shift)?;
        if start_pfn
            .as_usize()
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
        free_lists: &[SectionFrameNumber; MAX_ORDER + 1],
        meta: &[PageMeta],
        pfn: SectionFrameNumber,
    ) -> Option<(SectionFrameNumber, usize)> {
        for (order, free_list) in free_lists.iter().enumerate() {
            let block_pages = 1usize << order;
            let mut head = decode_pfn(*free_list);
            while let Some(block_pfn) = head {
                if pfn >= block_pfn && pfn.as_usize() < block_pfn.as_usize() + block_pages {
                    return Some((block_pfn, order));
                }
                head = decode_pfn(meta[block_pfn.as_usize()].next);
            }
        }
        None
    }

    /// Finds an aligned allocation start PFN inside a free block.
    ///
    /// Solves for the smallest section-local PFN `candidate` such that:
    ///
    /// - `candidate` is inside the free block `[block_pfn, block_pfn + 2^block_order)`
    /// - `candidate` is a valid `2^alloc_order` sub-block start inside that block
    /// - `heap_start_pfn + candidate` satisfies the requested byte alignment
    ///
    /// # Parameters
    ///
    /// - `heap_start_pfn`: The absolute physical PFN of section-local PFN 0. This
    ///   anchors section-local PFNs to real physical alignment.
    /// - `block_pfn`: The section-local PFN of the free block head currently being
    ///   examined. The returned candidate, if any, is within this block.
    /// - `block_order`: The buddy order of the free block headed by `block_pfn`.
    ///   The block contains `2^block_order` pages.
    /// - `alloc_order`: The requested allocation order. The allocation consumes
    ///   `2^alloc_order` contiguous pages, and the candidate must be a valid
    ///   sub-block start at this order.
    /// - `align`: The required byte alignment of the returned allocation address.
    ///   The caller guarantees that this is a power of two and at least the page
    ///   size.
    /// - `page_size_shift`: The number of low address bits occupied by the page
    ///   offset. The caller guarantees that `1 << page_size_shift` is the page
    ///   size.
    ///
    /// Returns `None` if no suitably aligned sub-block exists within the free
    /// block.
    fn find_aligned_pfn_in_block(
        heap_start_pfn: PhysFrameNumber,
        block_pfn: SectionFrameNumber,
        block_order: usize,
        alloc_order: usize,
        align: usize,
        page_size_shift: usize,
    ) -> Option<SectionFrameNumber> {
        // TODO: implement this function more efficiently.
        let alloc_pages = 1usize << alloc_order;
        let block_pages = 1usize << block_order;
        let last_start = block_pfn + (block_pages - alloc_pages);
        let align_pages = align >> page_size_shift;

        let heap_page_offset = heap_start_pfn.as_usize() & (align_pages - 1);
        let target_mod = (align_pages - heap_page_offset) & (align_pages - 1);
        let block_mod = block_pfn.as_usize() & (align_pages - 1);
        let step_mod = alloc_pages & (align_pages - 1);

        let mut residue = block_mod;
        for k in 0..align_pages {
            if residue == target_mod {
                let candidate = block_pfn + k * alloc_pages;
                return (candidate <= last_start).then_some(candidate);
            }
            residue = (residue + step_mod) & (align_pages - 1);
        }

        None
    }
}
