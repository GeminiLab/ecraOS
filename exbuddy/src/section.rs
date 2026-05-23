use core::{char::MAX, mem, ptr, slice};

use memory_addr::{VirtAddr, VirtAddrRange, align_up};

use crate::{
    MAX_ORDER,
    page_meta::{PFN_NONE, PageFlags, PageMeta, free_list_push},
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
}
