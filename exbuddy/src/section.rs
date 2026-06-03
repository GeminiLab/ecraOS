use core::{mem, ptr, slice};

use memory_addr::{MemoryAddr, PhysAddr, PhysAddrRange, align_up};

use crate::{
    MAX_ORDER,
    error::{BuddyError, BuddyResult},
    internal_error,
    page_meta::{PageFlags, PageMeta},
    pfn::{OptionSectionFrameNumber, PhysFrameNumber, SectionFrameNumber},
    stats::AllocatorStats,
};

/// Layout of a section.
pub struct SectionLayout {
    /// Total number of pages in the section.
    pub total_pages: usize,
    /// Number of pages used for metadata.
    pub metadata_pages: usize,
    /// Number of pages used for the heap.
    pub heap_pages: usize,
}

impl SectionLayout {
    /// Returns the range of the heap in physical addresses.
    pub fn heap_region(&self, base_addr: PhysAddr, page_size_shift: usize) -> PhysAddrRange {
        PhysAddrRange::new(
            base_addr + (self.metadata_pages << page_size_shift),
            base_addr + ((self.metadata_pages + self.heap_pages) << page_size_shift),
        )
    }
}

pub struct BuddySectionVisitor<'a> {
    metas: &'a [PageMeta],
    free_list_heads: &'a [OptionSectionFrameNumber; MAX_ORDER + 1],
    stats: &'a AllocatorStats,
    heap_base_pfn: PhysFrameNumber,
}

impl<'a> BuddySectionVisitor<'a> {
    /// Returns a read-only reference to the page metadata at given
    /// [`SectionFrameNumber`].
    #[inline]
    pub fn meta(&self, sfn: SectionFrameNumber) -> &PageMeta {
        &self.metas[sfn.as_usize()]
    }

    /// Returns the head of the free list for the given order.
    #[inline]
    pub fn free_list_head(&self, order: usize) -> Option<SectionFrameNumber> {
        self.free_list_heads[order].into_option_sfn()
    }

    /// Returns a reference to the allocator statistics.
    #[inline]
    pub fn stats(&self) -> &AllocatorStats {
        &self.stats
    }

    /// Finds the block head page containing the given page.
    pub fn find_block_containing(
        &self,
        sfn: SectionFrameNumber,
    ) -> BuddyResult<SectionFrameNumber> {
        if sfn.as_usize() >= self.stats.heap_pages {
            return Err(BuddyError::NotInHeap);
        }

        let mut order_head = sfn;
        for order in 0..=MAX_ORDER {
            if self.meta(order_head).flags != PageFlags::InBlock {
                return Ok(order_head);
            }

            let order_buddy = order_head.buddy(order, self.heap_base_pfn, self.stats.heap_pages).unwrap_or_else(|| {
                internal_error!("find_block_containing: buddy-less block {} (order {}) should never marked as in-block", order_head.as_usize(), order);
            });
            order_head = order_head.min(order_buddy);
        }

        internal_error!(
            "find_block_containing: page {} not in any block (last block checked is {})",
            sfn.as_usize(),
            order_head.as_usize()
        );
    }
}

pub struct BuddySectionVisitorMut<'a> {
    metas: &'a mut [PageMeta],
    free_list_heads: &'a mut [OptionSectionFrameNumber; MAX_ORDER + 1],
    stats: &'a mut AllocatorStats,
    heap_base_pfn: PhysFrameNumber,
}

impl<'a> BuddySectionVisitorMut<'a> {
    /// Returns a read-only visitor.
    #[inline]
    pub fn as_immut<'b>(&'b self) -> BuddySectionVisitor<'b>
    where
        'a: 'b,
    {
        BuddySectionVisitor {
            metas: self.metas,
            free_list_heads: self.free_list_heads,
            stats: self.stats,
            heap_base_pfn: self.heap_base_pfn,
        }
    }

    /// Returns a mutable reference to the page metadata at given
    /// [`SectionFrameNumber`].
    #[inline]
    pub fn meta_mut(&mut self, sfn: SectionFrameNumber) -> &mut PageMeta {
        &mut self.metas[sfn.as_usize()]
    }

    /// Sets the head of the free list for the given order.
    #[inline]
    pub fn set_free_list_head(&mut self, order: usize, sfn: Option<SectionFrameNumber>) {
        self.free_list_heads[order] = sfn.into();
    }

    /// Clears the page metadata array.
    pub fn clear_page_meta(&mut self) {
        // SAFETY: `meta` is a valid slice of `PageMeta`s. And, `PageMeta` can
        // be safely zero-initialized.
        unsafe {
            let page_data_byte_slice = slice::from_raw_parts_mut(
                self.metas.as_mut_ptr() as *mut u8,
                self.metas.len() * mem::size_of::<PageMeta>(),
            );
            page_data_byte_slice.fill(0);
        }
    }

    pub fn set_page_meta(&mut self, sfn: SectionFrameNumber, flags: PageFlags, order: usize) {
        let m = &mut self.metas[sfn.as_usize()];
        m.flags = flags;
        m.order = order as _;
    }

    pub fn free_list_head(&self, order: usize) -> Option<SectionFrameNumber> {
        self.free_list_heads[order].into_option_sfn()
    }

    #[inline]
    pub(super) fn free_list_push(&mut self, sfn: SectionFrameNumber, order: usize) {
        let old_head = self.free_list_heads[order].into_option_sfn();
        let m = &mut self.metas[sfn.as_usize()];
        m.clear_prev();
        m.set_next(old_head);
        if let Some(old_head) = old_head {
            self.metas[old_head.as_usize()].set_prev_to(sfn);
        }
        self.free_list_heads[order] = Some(sfn).into();
    }

    #[inline]
    pub(super) fn free_list_remove(&mut self, sfn: SectionFrameNumber, order: usize) {
        let m = &mut self.metas[sfn.as_usize()];
        let prev = m.prev();
        let next = m.next();

        if let Some(prev) = prev {
            self.metas[prev.as_usize()].set_next(next);
        } else {
            // pfn was the head
            self.free_list_heads[order] = next.into();
        }

        if let Some(next) = next {
            self.metas[next.as_usize()].set_prev(prev);
        }

        let m = &mut self.metas[sfn.as_usize()];
        m.clear_prev();
        m.clear_next();
    }

    /// Splits a free block into subblocks recursively and allocates a subblock
    /// at the given target [`SectionFrameNumber`].
    pub(super) fn split_and_alloc_subblock(
        &mut self,
        free_block_sfn: SectionFrameNumber,
        free_block_order: usize,
        target_sfn: SectionFrameNumber,
        target_order: usize,
    ) -> BuddyResult {
        debug_assert!(target_sfn >= free_block_sfn);
        debug_assert!(target_order <= free_block_order);
        debug_assert!(
            target_sfn.as_usize() + (1usize << target_order)
                <= free_block_sfn.as_usize() + (1usize << free_block_order)
        );

        self.free_list_remove(free_block_sfn, free_block_order);

        let mut current_order = free_block_order;
        let mut current_sfn = free_block_sfn;
        while current_order > target_order {
            current_order -= 1;
            let left_sfn = current_sfn;
            let right_sfn = current_sfn + (1 << current_order);
            let (next_sfn, free_sfn) = if target_sfn >= right_sfn {
                (right_sfn, left_sfn)
            } else {
                (left_sfn, right_sfn)
            };
            self.set_page_meta(free_sfn, PageFlags::Free, current_order);
            self.free_list_push(free_sfn, current_order);
            current_sfn = next_sfn;
        }

        self.set_page_meta(current_sfn, PageFlags::Allocated, target_order);
        self.stats.free_pages -= 1 << target_order;
        Ok(())
    }

    /// Frees a block of the given order and merges it with its buddies recursively.
    pub(super) fn free_and_merge_block(
        &mut self,
        sfn: SectionFrameNumber,
        order: usize,
    ) -> BuddyResult {
        let heap_base_pfn = self.heap_base_pfn;
        let mut current_order = order;
        let mut current_sfn = sfn;

        while current_order < MAX_ORDER {
            let Some(buddy_sfn) =
                current_sfn.buddy(current_order, heap_base_pfn, self.stats.heap_pages)
            else {
                break;
            };

            let buddy_m = self.meta_mut(buddy_sfn);
            if buddy_m.flags != PageFlags::Free || buddy_m.order as usize != current_order {
                break;
            }

            self.free_list_remove(buddy_sfn, current_order);
            self.set_page_meta(current_sfn, PageFlags::InBlock, 0);
            self.set_page_meta(buddy_sfn, PageFlags::InBlock, 0);

            current_sfn = current_sfn.min(buddy_sfn);
            current_order += 1;
        }

        self.set_page_meta(current_sfn, PageFlags::Free, current_order);
        self.free_list_push(current_sfn, current_order);
        self.stats.free_pages += 1 << order;
        Ok(())
    }
}

/// Section-local buddy state stored in the section header.
#[repr(C)]
pub struct BuddySection {
    /// Pointer to the next section in the linked list.
    pub next: *mut BuddySection,
    /// Full range of the section in physical addresses, including the header,
    /// the page metadata array, and the heap.
    pub region: PhysAddrRange,
    /// Range of the allocatable heap.
    ///
    /// On very rare occasions, `heap_region.end` may not be equal to `region.end`.
    pub heap_region: PhysAddrRange,
    /// Absolute physical frame number for the start of the heap.
    ///
    /// It's cached to avoid recalculating it on every allocation.
    pub heap_base_pfn: PhysFrameNumber,
    /// Usage statistics for the allocator.
    pub stats: AllocatorStats,
    /// Per-order free-list heads.
    pub free_list_heads: [OptionSectionFrameNumber; MAX_ORDER + 1],
}

/// Asserts that the section header is aligned to the page meta alignment, which
/// means, no padding bytes are needed between the section header and the
/// metadata array.
const _: () = assert!(mem::size_of::<BuddySection>().is_multiple_of(mem::align_of::<PageMeta>()));

/// Layout calculation.
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
        if total_bytes <= mem::size_of::<BuddySection>() {
            return 0;
        }
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
    pub const fn max_order_for_pfn(pfn: PhysFrameNumber) -> usize {
        let order = pfn.max_order();
        if order > MAX_ORDER { MAX_ORDER } else { order }
    }

    /// Returns the maximum buddy order that can be allocated within the heap.
    ///
    /// Computes the largest order for a section-local PFN by checking the
    /// absolute page-frame alignment and the remaining heap size.
    pub const fn max_order_for_pfn_in_heap(
        pfn: PhysFrameNumber,
        sfn: SectionFrameNumber,
        heap_pages: usize,
    ) -> usize {
        let mut order = Self::max_order_for_pfn(pfn);
        while order > 0 {
            if sfn.as_usize() + (1usize << order) <= heap_pages {
                break;
            }
            order -= 1;
        }
        order
    }
}

/// Initialization and visitor access.
impl BuddySection {
    /// Initializes a section at a given pointer.
    ///
    /// # Safety
    ///
    /// - The `ptr` must be a valid pointer to a `BuddySection`.
    /// - The `region` must be a valid and readable-writable virtual address
    ///   range.
    pub unsafe fn init_at(
        ptr: *mut BuddySection,
        region: PhysAddrRange,
        layout: SectionLayout,
        page_size_shift: usize,
    ) {
        let heap_region = layout.heap_region(region.start, page_size_shift);
        let heap_base_pfn = PhysFrameNumber::from_phys_addr(heap_region.start, page_size_shift);

        // SAFETY: the validity of the `ptr` is guaranteed by the caller.
        unsafe {
            ptr.write(BuddySection {
                next: ptr::null_mut(),
                region,
                heap_region,
                heap_base_pfn,
                free_list_heads: [OptionSectionFrameNumber::NONE; MAX_ORDER + 1],
                stats: AllocatorStats {
                    total_pages: layout.total_pages,
                    meta_pages: layout.metadata_pages,
                    heap_pages: layout.heap_pages,
                    free_pages: 0,
                },
            })
        };

        // Get the visitor
        let section = unsafe { ptr.as_mut_unchecked() };
        let mut visitor = section.visitor_mut();

        // Zero-initialize the page metadata array.
        visitor.clear_page_meta();

        // Initialize block heads.
        let heap_pages = layout.heap_pages;
        let mut sfn = SectionFrameNumber::new(0);
        while sfn.as_usize() < heap_pages {
            let pfn = heap_base_pfn + sfn.as_usize();
            let order = Self::max_order_for_pfn_in_heap(pfn, sfn, heap_pages);
            let block_pages = 1usize << order;

            visitor.set_page_meta(sfn, PageFlags::Free, order);
            visitor.free_list_push(sfn, order);

            visitor.stats.free_pages += block_pages;
            sfn += block_pages;
        }
    }

    pub fn visitor(&self) -> BuddySectionVisitor<'_> {
        let meta = {
            let self_ptr = self as *const _ as *const u8;
            let meta_ptr = unsafe { self_ptr.byte_add(mem::size_of::<Self>()) as *const PageMeta };
            unsafe { slice::from_raw_parts(meta_ptr, self.stats.heap_pages) }
        };
        let free_lists = &self.free_list_heads;
        let stats = &self.stats;
        BuddySectionVisitor {
            metas: meta,
            free_list_heads: free_lists,
            stats,
            heap_base_pfn: self.heap_base_pfn,
        }
    }

    pub fn visitor_mut(&mut self) -> BuddySectionVisitorMut<'_> {
        let meta = {
            let self_ptr = self as *mut _ as *mut u8;
            let meta_ptr = unsafe { self_ptr.byte_add(mem::size_of::<Self>()) as *mut PageMeta };
            unsafe { slice::from_raw_parts_mut(meta_ptr, self.stats.heap_pages) }
        };
        let free_lists = &mut self.free_list_heads;
        let stats = &mut self.stats;
        BuddySectionVisitorMut {
            metas: meta,
            free_list_heads: free_lists,
            stats,
            heap_base_pfn: self.heap_base_pfn,
        }
    }
}

/// Allocation and deallocation.
impl BuddySection {
    /// Allocates a free block of the given order and alignment.
    pub fn alloc_block(
        &mut self,
        order: usize,
        align: usize,
        page_size_shift: usize,
    ) -> BuddyResult<PhysAddr> {
        if order > MAX_ORDER {
            return Err(BuddyError::InvalidOrder);
        }

        let page_size = 1usize << page_size_shift;
        if !align.is_power_of_two() || align < page_size {
            return Err(BuddyError::InvalidAlignment);
        }

        let align_pages = align >> page_size_shift;

        let heap_base_pfn = self.heap_base_pfn;
        let mut visitor = self.visitor_mut();
        // Search from the requested order to the maximum order.
        for free_block_order in order..=MAX_ORDER {
            // Iterate over the free list for the current order.
            let mut free_block = visitor.free_list_head(free_block_order);
            while let Some(free_block_sfn) = free_block {
                let free_block_pfn = heap_base_pfn + free_block_sfn.as_usize();

                // Check the expected invariant: all blocks' pa are naturally aligned to
                // their max order (and therefore to all its possible orders).
                debug_assert!(
                    free_block_pfn
                        .to_phys_addr(page_size_shift)
                        .is_aligned(1usize << (page_size_shift + free_block_order))
                );

                // The free block head is aligned if:
                // - The free block's current order is greater than the alignment, or
                // - The free block happens to be aligned to the alignment.
                if 1usize << free_block_order >= align_pages
                    || free_block_pfn.as_usize().is_multiple_of(align_pages)
                {
                    let target_sfn = free_block_sfn;

                    visitor.split_and_alloc_subblock(
                        free_block_sfn,
                        free_block_order,
                        target_sfn,
                        order,
                    )?;

                    return Ok(target_sfn
                        .to_pfn(self.heap_base_pfn)
                        .to_phys_addr(page_size_shift));
                }
                free_block = visitor.metas[free_block_sfn.as_usize()].next();
            }
        }

        Err(BuddyError::NoMemory)
    }

    /// Deallocates a block of the given order and alignment.
    pub fn dealloc_block(
        &mut self,
        addr: PhysAddr,
        order: usize,
        page_size_shift: usize,
    ) -> BuddyResult {
        if order > MAX_ORDER {
            return Err(BuddyError::InvalidOrder);
        }

        let page_size = 1usize << page_size_shift;
        if !addr.is_aligned(page_size) {
            return Err(BuddyError::NotAligned);
        }

        let pfn = PhysFrameNumber::from_phys_addr(addr, page_size_shift);
        let sfn = SectionFrameNumber::from_pfn(pfn, self.heap_base_pfn);

        let visitor = self.visitor();
        let m = visitor.meta(sfn);

        if m.flags == PageFlags::InBlock || m.order as usize != order {
            return Err(BuddyError::OrderMismatch);
        }

        if m.flags != PageFlags::Allocated {
            return Err(BuddyError::NotAllocated);
        }

        let mut visitor = self.visitor_mut();

        visitor.free_and_merge_block(sfn, order)
    }

    /// Iterates over the blocks in the given range. Returns an accumulated
    /// result from the initial value and the accumulating closure.
    ///
    /// The following arguments will be passed to the `f` closure:
    /// - The accumulator.
    /// - The mutable visitor to the section's page metadata.
    /// - The head page of the block.
    /// - The order of the block.
    /// - Whether the block is obtained from the `block_splitter` or not.
    ///
    /// If either end of the `range` is not aligned to page border, the function
    /// will return an error.
    ///
    /// If the `range` intersects, but does not contain a block entirely, this
    /// function will call `block_splitter` to split the block into smaller
    /// blocks. The following arguments will be passed to the `block_splitter`:
    /// - The mutable visitor to the section's page metadata.
    /// - The head page of the block.
    /// - The order of the block.
    /// - The start frame number of the expected block.
    /// - The order of the expected block.
    ///
    /// The `block_splitter` should return two values:
    /// - The actual head page that should be passed to the `f` closure.
    /// - The actual order that should be passed to the `f` closure.
    /// These values may be different from the arguments passed to the
    /// `block_splitter`. The start position of the next iteration will be
    /// inferred from these values.
    fn iter_blocks_in_range<'a, T, S, F>(
        &'a mut self,
        range: PhysAddrRange,
        page_size_shift: usize,
        mut block_splitter: S,
        init: T,
        mut f: F,
    ) -> BuddyResult<T>
    where
        S: FnMut(
            &mut BuddySectionVisitorMut<'a>,
            SectionFrameNumber,
            usize,
            SectionFrameNumber,
            usize,
        ) -> BuddyResult<(SectionFrameNumber, usize)>,
        F: FnMut(
            T,
            &mut BuddySectionVisitorMut<'a>,
            SectionFrameNumber,
            usize,
            bool,
        ) -> BuddyResult<T>,
    {
        if !self.heap_region.contains_range(range) {
            return Err(BuddyError::NotInHeap);
        }

        let page_size = 1usize << page_size_shift;
        if !range.start.is_aligned(page_size) || !range.end.is_aligned(page_size) {
            return Err(BuddyError::NotAligned);
        }

        let heap_base_pfn = self.heap_base_pfn;
        let start_pfn = PhysFrameNumber::from_phys_addr(range.start, page_size_shift);
        let start_sfn = SectionFrameNumber::from_pfn(start_pfn, heap_base_pfn);
        let end_pfn = PhysFrameNumber::from_phys_addr(range.end, page_size_shift);
        let end_sfn = SectionFrameNumber::from_pfn(end_pfn, heap_base_pfn);

        let mut visitor = self.visitor_mut();
        let mut acc = init;
        let mut current_sfn = start_sfn;
        while current_sfn < end_sfn {
            let pages_remaining = end_sfn - current_sfn;
            let block = visitor.as_immut().find_block_containing(current_sfn)?;
            let block_order = visitor.metas[block.as_usize()].order as usize;
            let block_end_sfn = block + (1usize << block_order);

            if block == current_sfn && block_end_sfn <= end_sfn {
                // This block is fully contained in the range.
                acc = f(acc, &mut visitor, block, block_order, false)?;
                current_sfn = block_end_sfn;
            } else if block == current_sfn && block_end_sfn > end_sfn {
                // Only the start of the block is in the range.
                let expected_order = crate::page_count_to_order_floor(pages_remaining)?;

                // We don't need to check whether `expected_order` is greater
                // than the block's order, because if it is, the block will be
                // fully contained in the range.
                let (actual_sfn, actual_order) = block_splitter(
                    &mut visitor,
                    block,
                    block_order,
                    current_sfn,
                    expected_order,
                )?;

                acc = f(acc, &mut visitor, actual_sfn, actual_order, true)?;
                current_sfn = actual_sfn + (1usize << actual_order);
            } else if block < current_sfn {
                // Only the end of the block is in the range when (block_end_sfn
                // <= end_sfn), or, The range is completely contained in the
                // block (block_end_sfn > end_sfn)

                let current_order = Self::max_order_for_pfn_in_heap(
                    heap_base_pfn + current_sfn.as_usize(),
                    current_sfn,
                    block_end_sfn.min(end_sfn).as_usize(),
                );

                let (actual_sfn, actual_order) =
                    block_splitter(&mut visitor, block, block_order, current_sfn, current_order)?;

                acc = f(acc, &mut visitor, actual_sfn, actual_order, true)?;
                current_sfn = actual_sfn + (1usize << actual_order);
            } else {
                internal_error!(
                    "iter_blocks_in_range: relationship between block {} and sfn range {:?}..{:?} is not possible",
                    block.as_usize(),
                    current_sfn,
                    end_sfn
                );
            }
        }

        Ok(acc)
    }

    /// Allocates a range of free blocks at the given address.
    pub fn alloc_blocks_at(
        &mut self,
        addr: PhysAddr,
        count: usize,
        page_size_shift: usize,
    ) -> BuddyResult {
        if count == 0 {
            return Err(BuddyError::InvalidPageCount);
        }

        let range = PhysAddrRange::from_start_size(addr, count << page_size_shift);

        // Check whether the range is free
        self.iter_blocks_in_range(
            range,
            page_size_shift,
            |_, block, block_order, _, _| Ok((block, block_order)),
            (),
            |_, visitor, block, _, _| {
                if visitor.metas[block.as_usize()].flags != PageFlags::Free {
                    return Err(BuddyError::AlreadyAllocated);
                }

                Ok(())
            },
        )?;

        self.iter_blocks_in_range(
            PhysAddrRange::from_start_size(addr, count << page_size_shift),
            page_size_shift,
            |visitor,
             free_block_sfn: SectionFrameNumber,
             free_block_order: usize,
             target_sfn: SectionFrameNumber,
             target_order: usize| {
                debug_assert_eq!(
                    visitor.metas[free_block_sfn.as_usize()].flags,
                    PageFlags::Free
                );

                visitor.split_and_alloc_subblock(
                    free_block_sfn,
                    free_block_order,
                    target_sfn,
                    target_order,
                )?;
                Ok((target_sfn, target_order))
            },
            (),
            |_, visitor, block, order, splitted| {
                if !splitted {
                    debug_assert_eq!(visitor.metas[block.as_usize()].flags, PageFlags::Free);

                    visitor.set_page_meta(block, PageFlags::Allocated, order);
                    visitor.free_list_remove(block, order);
                    visitor.stats.free_pages -= 1 << order;
                }

                Ok(())
            },
        )
    }

    /// Deallocates a range of allocated blocks allocated by [`alloc_blocks_at`](Self::alloc_blocks_at).
    pub fn dealloc_blocks_at(
        &mut self,
        addr: PhysAddr,
        count: usize,
        page_size_shift: usize,
    ) -> BuddyResult {
        if count == 0 {
            return Err(BuddyError::InvalidPageCount);
        }

        let range = PhysAddrRange::from_start_size(addr, count << page_size_shift);

        self.iter_blocks_in_range(
            range,
            page_size_shift,
            |_, _, _, _, _| Err(BuddyError::OrderMismatch),
            (),
            |_acc, visitor, block, _, splitted| {
                debug_assert!(!splitted);
                if visitor.metas[block.as_usize()].flags != PageFlags::Allocated {
                    return Err(BuddyError::NotAllocated);
                }

                Ok(())
            },
        )?;

        self.iter_blocks_in_range(
            range,
            page_size_shift,
            |_, _, _, _, _| Err(BuddyError::OrderMismatch),
            (),
            |_acc, visitor, block, order, splitted| {
                debug_assert!(!splitted);
                visitor.free_and_merge_block(block, order)
            },
        )
    }

    /// Returns whether the page containing the physical address is allocated.
    ///
    /// Unlike other functions, this function does not require the `addr` to be
    /// aligned to page border.
    pub fn is_allocated(&self, addr: PhysAddr, page_size_shift: usize) -> BuddyResult<bool> {
        let sfn = SectionFrameNumber::from_pfn(
            PhysFrameNumber::from_phys_addr(addr, page_size_shift),
            self.heap_base_pfn,
        );
        let visitor = self.visitor();
        let head = visitor.find_block_containing(sfn)?;

        Ok(visitor.meta(head).flags == PageFlags::Allocated)
    }

    /// Checks if the given virtual address falls within this section's heap.
    #[inline]
    pub fn contains_heap_addr(&self, addr: PhysAddr) -> bool {
        self.heap_region.contains(addr)
    }

    /// Returns a read-only summary of this section.
    #[inline]
    pub fn stats(&self) -> AllocatorStats {
        self.stats.clone()
    }
}
