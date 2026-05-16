//! Buddy page allocator.
//!
//! Provides a page-level physical memory allocator using the buddy system
//! algorithm. Metadata and per-section state are stored in a prefix of each
//! managed memory region, requiring no dynamic allocation.

use core::ptr;

use memory_addr::{PhysAddr, PhysAddrRange, align_up, is_aligned};

use crate::error::{AllocError, AllocResult};
use crate::page_meta::{PFN_NONE, PageFlags, PageMeta, free_list_push, free_list_remove};

/// Maximum buddy order.
///
/// With 4 KiB pages this gives 2^20 x 4 KiB = 4 GiB blocks.
pub const MAX_ORDER: usize = 20;

/// DMA32 zone upper bound (4 GiB physical).
const DMA32_LIMIT: usize = 0x1_0000_0000;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Aligns and clamps a memory region to the given granule.
///
/// Returns `None` if the region is empty, the granule is not a power of two,
/// or the aligned region has zero size.
fn normalize_region(
    region_start: usize,
    region_size: usize,
    granule: usize,
) -> Option<(usize, usize)> {
    if region_size == 0 || !granule.is_power_of_two() {
        return None;
    }
    let region_end = region_start.checked_add(region_size)?;
    let usable_start = align_up(region_start, granule);
    let usable_end = region_end & !(granule - 1);
    if usable_end <= usable_start {
        return None;
    }
    Some((usable_start, usable_end - usable_start))
}

// ---------------------------------------------------------------------------
// Layout and init specification types
// ---------------------------------------------------------------------------

/// Describes the computed layout of a buddy section within a memory region.
pub(crate) struct RegionLayout {
    /// Virtual address where the [`BuddySection`] header is placed.
    pub(crate) section_start: usize,
    /// Virtual address where the [`PageMeta`] array begins.
    pub(crate) meta_start: usize,
    /// Virtual address where the allocatable heap begins.
    pub(crate) managed_heap_start: usize,
    /// Size in bytes of the allocatable heap.
    pub(crate) managed_heap_size: usize,
}

/// Carries all parameters needed to initialize a single [`BuddySection`].
pub(crate) struct SectionInitSpec {
    /// Virtual address of the start of the containing memory region.
    pub(crate) region_start: usize,
    /// Size in bytes of the containing memory region.
    pub(crate) region_size: usize,
    /// Pointer to the uninitialized [`BuddySection`] storage.
    pub(crate) section_ptr: *mut BuddySection,
    /// Pointer to the uninitialized [`PageMeta`] array storage.
    pub(crate) meta_ptr: *mut u8,
    /// Size in bytes reserved for the [`PageMeta`] array.
    pub(crate) meta_size: usize,
    /// Virtual address of the start of the allocatable heap.
    pub(crate) heap_start: usize,
    /// Size in bytes of the allocatable heap.
    pub(crate) heap_size: usize,
}

// ---------------------------------------------------------------------------
// Public summary types
// ---------------------------------------------------------------------------

/// Read-only summary of a managed section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedSection {
    /// Start physical address of the managed section heap.
    pub start: usize,
    /// Size in bytes of the managed section heap.
    pub size: usize,
    /// Number of free pages in the managed section.
    pub free_pages: usize,
    /// Total number of pages in the managed section.
    pub total_pages: usize,
}

/// Usage statistics for the allocator.
pub struct AllocatorUsage {
    /// Total number of pages managed across all sections.
    pub total_pages: usize,
    /// Number of pages currently in use across all sections.
    pub used_pages: usize,
}

// ---------------------------------------------------------------------------
// BuddySection
// ---------------------------------------------------------------------------

/// Per-region buddy state stored in the region prefix.
///
/// All addresses stored in this struct are virtual addresses (raw `usize`).
#[repr(C)]
pub(crate) struct BuddySection {
    /// Pointer to the next section in the linked list.
    pub(crate) next: *mut BuddySection,
    /// Virtual address of the start of the containing memory region.
    pub(crate) region_start: usize,
    /// Size in bytes of the containing memory region.
    pub(crate) region_size: usize,
    /// Pointer to the [`PageMeta`] array for this section.
    pub(crate) meta: *mut PageMeta,
    /// Maximum number of pages that can be tracked in this section.
    pub(crate) max_pages: usize,
    /// Virtual address of the start of the allocatable heap.
    pub(crate) heap_start: usize,
    /// Size in bytes of the allocatable heap.
    pub(crate) heap_size: usize,
    /// Per-order free-list heads (PFN indices).
    pub(crate) free_lists: [u32; MAX_ORDER + 1],
    /// Number of currently free pages in this section.
    pub(crate) free_pages: usize,
    /// Total number of pages in this section.
    pub(crate) total_pages: usize,
}

impl BuddySection {
    /// Returns the alignment required for the section header and metadata.
    const fn metadata_align() -> usize {
        let section_align = core::mem::align_of::<BuddySection>();
        let meta_align = core::mem::align_of::<PageMeta>();
        if section_align > meta_align {
            section_align
        } else {
            meta_align
        }
    }

    /// Computes the metadata offset and total metadata size for a given page count.
    fn metadata_layout_for_pages(pages: usize) -> Option<(usize, usize)> {
        let meta_offset = align_up(
            core::mem::size_of::<BuddySection>(),
            core::mem::align_of::<PageMeta>(),
        );
        let page_meta_size = pages.checked_mul(core::mem::size_of::<PageMeta>())?;
        let meta_size = meta_offset.checked_add(page_meta_size)?;
        Some((meta_offset, meta_size))
    }

    /// Computes the number of available heap pages given the region end,
    /// section start, metadata size, and heap alignment.
    fn available_heap_pages(
        region_end: usize,
        section_start: usize,
        meta_size: usize,
        heap_align: usize,
        page_size: usize,
    ) -> Option<usize> {
        let managed_heap_start = align_up(section_start.checked_add(meta_size)?, heap_align);
        if managed_heap_start > region_end {
            return Some(0);
        }
        Some((region_end - managed_heap_start) / page_size)
    }

    /// Checks whether a given number of pages can be managed within the region.
    fn can_manage_pages(
        region_end: usize,
        section_start: usize,
        pages: usize,
        heap_align: usize,
        page_size: usize,
    ) -> bool {
        let Some((_, meta_size)) = Self::metadata_layout_for_pages(pages) else {
            return false;
        };
        let Some(available_pages) =
            Self::available_heap_pages(region_end, section_start, meta_size, heap_align, page_size)
        else {
            return false;
        };
        available_pages >= pages
    }

    /// Computes the optimal region layout for a given page size and heap alignment.
    ///
    /// Uses binary search to find the maximum number of pages that can be managed
    /// while fitting both the section header and the page metadata array within
    /// the region prefix.
    pub(crate) fn compute_region_layout_with_heap_align(
        region_start: usize,
        region_size: usize,
        page_size: usize,
        heap_align: usize,
    ) -> Option<RegionLayout> {
        if region_size == 0 || !page_size.is_power_of_two() || !heap_align.is_power_of_two() {
            return None;
        }

        let region_end = region_start.checked_add(region_size)?;
        let section_start = align_up(region_start, Self::metadata_align());
        if section_start >= region_end {
            return None;
        }

        let heap_search_start = align_up(
            section_start.checked_add(core::mem::size_of::<BuddySection>())?,
            page_size,
        );
        let max_pages = if heap_search_start >= region_end {
            0
        } else {
            (region_end - heap_search_start) / page_size
        };

        let mut low = 0usize;
        let mut high = max_pages;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if Self::can_manage_pages(region_end, section_start, mid, heap_align, page_size) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }

        if low == 0 {
            return None;
        }

        let (meta_offset, meta_size) = Self::metadata_layout_for_pages(low)?;
        let meta_start = section_start.checked_add(meta_offset)?;
        let managed_heap_start = align_up(section_start.checked_add(meta_size)?, heap_align);
        let managed_heap_size = low.checked_mul(page_size)?;

        Some(RegionLayout {
            section_start,
            meta_start,
            managed_heap_start,
            managed_heap_size,
        })
    }

    /// Computes the optimal region layout with heap alignment equal to `page_size`.
    fn compute_region_layout(
        region_start: usize,
        region_size: usize,
        page_size: usize,
    ) -> Option<RegionLayout> {
        Self::compute_region_layout_with_heap_align(region_start, region_size, page_size, page_size)
    }

    /// Initializes a buddy section at the given location.
    ///
    /// Writes the [`BuddySection`] header, initializes the [`PageMeta`] array,
    /// and populates the free lists by scanning the heap in buddy-order blocks.
    ///
    /// # Safety
    ///
    /// - `section_ptr` must point to writable memory of sufficient size.
    /// - `meta_ptr` must point to writable memory of at least `meta_size` bytes.
    /// - The heap region `[heap_start, heap_start + heap_size)` must be valid
    ///   and writable.
    #[allow(clippy::too_many_arguments)]
    unsafe fn init_at(
        section_ptr: *mut BuddySection,
        region_start: usize,
        region_size: usize,
        meta_ptr: *mut u8,
        meta_size: usize,
        heap_start: usize,
        heap_size: usize,
        page_size: usize,
    ) -> AllocResult {
        unsafe {
            if !page_size.is_power_of_two() {
                return Err(AllocError::InvalidParam);
            }
            if !is_aligned(heap_start, page_size) || heap_size == 0 {
                return Err(AllocError::InvalidParam);
            }

            let total_pages = heap_size / page_size;
            let required = BuddyAllocator::required_meta_size(heap_size, page_size);
            if meta_size < required {
                return Err(AllocError::InvalidParam);
            }

            let meta = meta_ptr as *mut PageMeta;
            for i in 0..total_pages {
                meta.add(i).write(PageMeta::new());
            }

            section_ptr.write(BuddySection {
                next: ptr::null_mut(),
                region_start,
                region_size,
                meta,
                max_pages: total_pages,
                heap_start,
                heap_size,
                free_lists: [PFN_NONE; MAX_ORDER + 1],
                free_pages: 0,
                total_pages,
            });

            let section = &mut *section_ptr;
            let mut pfn: usize = 0;
            while pfn < total_pages {
                let mut order = MAX_ORDER;
                loop {
                    let block_pages = 1usize << order;
                    if block_pages <= total_pages - pfn && (pfn & (block_pages - 1)) == 0 {
                        break;
                    }
                    if order == 0 {
                        break;
                    }
                    order -= 1;
                }
                let block_pages = 1usize << order;
                let m = &mut *section.meta.add(pfn);
                m.flags = PageFlags::Free;
                m.order = order as u8;
                free_list_push(section.meta, &mut section.free_lists, pfn as u32, order);
                section.free_pages += block_pages;
                pfn += block_pages;
            }

            Ok(())
        }
    }

    /// Returns `true` if the given virtual address falls within this section's heap.
    #[inline]
    fn contains_heap_addr(&self, addr: usize) -> bool {
        addr >= self.heap_start && addr < self.heap_start + self.heap_size
    }

    /// Produces a read-only summary of this section.
    #[inline]
    fn summary(&self) -> ManagedSection {
        ManagedSection {
            start: self.heap_start,
            size: self.heap_size,
            free_pages: self.free_pages,
            total_pages: self.total_pages,
        }
    }
}

// ---------------------------------------------------------------------------
// BuddyAllocator
// ---------------------------------------------------------------------------

/// Page-metadata-based buddy allocator for physical memory.
///
/// Stores `page_size` and `virt_phys_offset` as runtime fields rather than
/// const generics, so a single type can serve different configurations.
/// The public API operates on [`PhysAddr`] values; internally all section
/// addresses are virtual.
pub struct BuddyAllocator {
    /// Page size in bytes (must be a power of two).
    page_size: usize,
    /// Offset added to a physical address to obtain the corresponding virtual address.
    virt_phys_offset: usize,
    /// Head of the intrusive linked list of managed sections.
    sections_head: *mut BuddySection,
    /// Tail of the intrusive linked list of managed sections.
    sections_tail: *mut BuddySection,
    /// Number of managed sections.
    section_count: usize,
}

// SAFETY: The allocator is designed to be wrapped in a SpinMutex.
// All section pointers point into caller-provided regions whose lifetime
// is managed externally.
unsafe impl Send for BuddyAllocator {}

impl BuddyAllocator {
    /// Calculates the metadata-region size (in bytes) required for `heap_size`
    /// bytes of heap with the given page size.
    pub const fn required_meta_size(heap_size: usize, page_size: usize) -> usize {
        let pages = heap_size / page_size;
        pages * core::mem::size_of::<PageMeta>()
    }

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

    /// Converts a physical address to a virtual address.
    #[inline]
    fn phys_to_virt(&self, paddr: usize) -> usize {
        paddr + self.virt_phys_offset
    }

    /// Converts a virtual address to a physical address.
    #[inline]
    fn virt_to_phys(&self, vaddr: usize) -> usize {
        vaddr - self.virt_phys_offset
    }
}

impl Default for BuddyAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl BuddyAllocator {
    /// Resets the allocator to the uninitialised state.
    pub(crate) fn reset(&mut self) {
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
    /// - Bytes consumed by metadata become unavailable for allocation.
    pub unsafe fn init(
        &mut self,
        region: PhysAddrRange,
        page_size: usize,
        virt_phys_offset: usize,
    ) -> AllocResult {
        unsafe {
            self.page_size = page_size;
            self.virt_phys_offset = virt_phys_offset;
            self.reset();
            self.add_region(region)
        }
    }

    /// Adds a new managed memory region after initialisation.
    ///
    /// The region is given as a physical address range and is converted to
    /// virtual addresses internally.
    ///
    /// # Safety
    ///
    /// - The physical memory region must be writable (mapped) and remain valid
    ///   for the lifetime of this allocator.
    /// - The region must not overlap any existing managed region.
    pub unsafe fn add_region(&mut self, region: PhysAddrRange) -> AllocResult {
        unsafe {
            let region_start_phys = region.start.as_usize();
            let region_size = region.size();
            let region_start_virt = self.phys_to_virt(region_start_phys);

            let (region_start, region_size) =
                normalize_region(region_start_virt, region_size, self.page_size)
                    .ok_or(AllocError::InvalidParam)?;
            let layout =
                BuddySection::compute_region_layout(region_start, region_size, self.page_size)
                    .ok_or(AllocError::InvalidParam)?;

            self.add_region_raw(SectionInitSpec {
                region_start,
                region_size,
                section_ptr: layout.section_start as *mut BuddySection,
                meta_ptr: layout.meta_start as *mut u8,
                meta_size: Self::required_meta_size(layout.managed_heap_size, self.page_size),
                heap_start: layout.managed_heap_start,
                heap_size: layout.managed_heap_size,
            })
        }
    }

    /// Adds a region using pre-computed layout parameters.
    ///
    /// Checks for overlap with existing sections and initializes the new section.
    ///
    /// # Safety
    ///
    /// All pointers in `spec` must be valid and the memory regions must be
    /// writable for the lifetime of the allocator.
    pub(crate) unsafe fn add_region_raw(&mut self, spec: SectionInitSpec) -> AllocResult {
        unsafe {
            let region_size = spec.region_size;
            let region_end = spec
                .region_start
                .checked_add(region_size)
                .ok_or(AllocError::InvalidParam)?;
            let heap_end = spec
                .heap_start
                .checked_add(spec.heap_size)
                .ok_or(AllocError::InvalidParam)?;
            if heap_end > region_end {
                return Err(AllocError::InvalidParam);
            }

            // Check for overlap with existing sections.
            let mut section = self.sections_head;
            while !section.is_null() {
                let existing = &*section;
                let existing_end = existing
                    .region_start
                    .checked_add(existing.region_size)
                    .ok_or(AllocError::InvalidParam)?;
                if spec.region_start < existing_end && existing.region_start < region_end {
                    return Err(AllocError::MemoryOverlap);
                }
                section = existing.next;
            }

            BuddySection::init_at(
                spec.section_ptr,
                spec.region_start,
                spec.region_size,
                spec.meta_ptr,
                spec.meta_size,
                spec.heap_start,
                spec.heap_size,
                self.page_size,
            )?;

            if self.sections_head.is_null() {
                self.sections_head = spec.section_ptr;
            } else {
                (*self.sections_tail).next = spec.section_ptr;
            }
            self.sections_tail = spec.section_ptr;
            self.section_count += 1;

            Ok(())
        }
    }

    /// Returns the number of managed sections.
    pub fn section_count(&self) -> usize {
        self.section_count
    }

    /// Returns a read-only summary for a managed section by registration order.
    pub fn section(&self, index: usize) -> Option<ManagedSection> {
        let mut current = self.sections_head;
        let mut i = 0usize;
        while !current.is_null() {
            if i == index {
                return Some(unsafe { (&*current).summary() });
            }
            current = unsafe { (*current).next };
            i += 1;
        }
        None
    }

    /// Returns the total number of pages managed across all sections.
    pub fn total_pages(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).total_pages };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Returns the total managed heap bytes across all sections.
    ///
    /// This counts only bytes in allocatable heaps, excluding region-prefix
    /// metadata.
    pub fn managed_bytes(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).heap_size };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Returns the number of currently free pages across all sections.
    pub fn free_pages(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).free_pages };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Returns the allocated backend bytes across all sections.
    ///
    /// This is computed as managed heap bytes minus currently free page bytes.
    /// It reflects page-level occupancy, so it includes slab pages, alignment
    /// amplification, and internal fragmentation.
    pub fn allocated_bytes(&self) -> usize {
        self.managed_bytes()
            .saturating_sub(self.free_pages().saturating_mul(self.page_size))
    }

    /// Allocates `count` contiguous physical frames with the given alignment.
    ///
    /// Returns the starting physical address of the allocation on success.
    pub fn alloc_frames(&mut self, count: usize, align: usize) -> AllocResult<PhysAddr> {
        if count == 0 {
            return Err(AllocError::InvalidParam);
        }
        let align = if align == 0 { self.page_size } else { align };
        if !align.is_power_of_two() || align < self.page_size {
            return Err(AllocError::InvalidParam);
        }

        let order = count.next_power_of_two().trailing_zeros() as usize;
        if order > MAX_ORDER {
            return Err(AllocError::InvalidParam);
        }

        let mut section = self.sections_head;
        while !section.is_null() {
            if let Ok(vaddr) = unsafe {
                Self::alloc_from_section_aligned(&mut *section, order, align, self.page_size)
            } {
                return Ok(PhysAddr::from(self.virt_to_phys(vaddr)));
            }
            section = unsafe { (*section).next };
        }

        Err(AllocError::NoMemory)
    }

    /// Allocates a single physical frame.
    ///
    /// This is a convenience wrapper around [`alloc_frames`](Self::alloc_frames).
    pub fn alloc_frame(&mut self) -> AllocResult<PhysAddr> {
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
    ) -> AllocResult<PhysAddr> {
        unsafe {
            if count == 0 {
                return Err(AllocError::InvalidParam);
            }

            let vaddr = self.phys_to_virt(paddr.as_usize());
            let page_size = self.page_size;
            let section = self
                .find_section_by_addr_mut(vaddr)
                .ok_or(AllocError::NotFound)?;

            let start_pfn = (vaddr - section.heap_start) / page_size;

            if start_pfn
                .checked_add(count)
                .ok_or(AllocError::InvalidParam)?
                > section.max_pages
            {
                return Err(AllocError::InvalidParam);
            }

            // Check that all target pages are free.
            for pfn in start_pfn..start_pfn + count {
                let m = &*section.meta.add(pfn);
                if m.flags != PageFlags::Free {
                    return Err(AllocError::NoMemory);
                }
            }

            // For each target PFN, find its containing free block and split down
            // to order 0.
            for target_pfn in start_pfn..start_pfn + count {
                let m = &*section.meta.add(target_pfn);
                let current_order = m.order as usize;

                // Only the head PFN of a free block should be processed.
                // If this PFN is not the head, find the head.
                let block_pfn = target_pfn & !((1usize << current_order) - 1);
                let head_pfn = block_pfn as u32;

                // Remove the block from its free list.
                free_list_remove(
                    section.meta,
                    &mut section.free_lists,
                    head_pfn,
                    current_order,
                );

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
                    let fm = &mut *section.meta.add(free_pfn);
                    fm.flags = PageFlags::Free;
                    fm.order = cur_order as u8;
                    free_list_push(
                        section.meta,
                        &mut section.free_lists,
                        free_pfn as u32,
                        cur_order,
                    );
                    cur_pfn = next_pfn;
                }

                // Mark the target page as allocated.
                let tm = &mut *section.meta.add(target_pfn);
                tm.flags = PageFlags::Allocated;
                tm.order = 0;
                section.free_pages -= 1;
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
        let vaddr = self.phys_to_virt(addr.as_usize());
        let page_size = self.page_size;
        let Some(section) = self.find_section_by_addr_mut(vaddr) else {
            debug_assert!(
                false,
                "dealloc_frames called with address outside all sections"
            );
            return;
        };

        debug_assert!(is_aligned(vaddr, page_size));
        debug_assert!(count > 0);

        let pfn = (vaddr - section.heap_start) / page_size;
        debug_assert!(pfn < section.max_pages);
        let stored = unsafe { &*section.meta.add(pfn) };
        debug_assert!(
            stored.flags == PageFlags::Allocated || stored.flags == PageFlags::Slab,
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
    pub unsafe fn set_page_flags(&mut self, addr: PhysAddr, flags: PageFlags) -> AllocResult {
        unsafe {
            let vaddr = self.phys_to_virt(addr.as_usize());
            let page_size = self.page_size;
            let section = self
                .find_section_by_addr_mut(vaddr)
                .ok_or(AllocError::NotFound)?;
            let pfn = (vaddr - section.heap_start) / page_size;
            (*section.meta.add(pfn)).flags = flags;
            Ok(())
        }
    }

    /// Returns the flags of the page containing the given physical address.
    pub fn page_flags(&self, addr: PhysAddr) -> AllocResult<PageFlags> {
        let vaddr = self.phys_to_virt(addr.as_usize());
        let section = self
            .find_section_by_addr(vaddr)
            .ok_or(AllocError::NotFound)?;
        let pfn = (vaddr - section.heap_start) / self.page_size;
        Ok(unsafe { (*section.meta.add(pfn)).flags })
    }

    /// Returns usage statistics for the allocator.
    pub fn usage(&self) -> AllocatorUsage {
        let total = self.total_pages();
        let free = self.free_pages();
        AllocatorUsage {
            total_pages: total,
            used_pages: total - free,
        }
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
    ) -> AllocResult<usize> {
        for search_order in order..=MAX_ORDER {
            let mut pfn_u32 = section.free_lists[search_order];
            while pfn_u32 != PFN_NONE {
                let block_pfn = pfn_u32 as usize;
                if let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    section.heap_start,
                    block_pfn,
                    search_order,
                    order,
                    align,
                    page_size,
                ) {
                    unsafe {
                        free_list_remove(
                            section.meta,
                            &mut section.free_lists,
                            pfn_u32,
                            search_order,
                        );
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
                            let bm = &mut *section.meta.add(free_pfn);
                            bm.flags = PageFlags::Free;
                            bm.order = current_order as u8;
                            free_list_push(
                                section.meta,
                                &mut section.free_lists,
                                free_pfn as u32,
                                current_order,
                            );
                        }
                        current_pfn = next_pfn;
                    }

                    unsafe {
                        let m = &mut *section.meta.add(current_pfn);
                        m.flags = PageFlags::Allocated;
                        m.order = order as u8;
                    }

                    section.free_pages -= 1 << order;
                    return Ok(section.heap_start + current_pfn * page_size);
                }
                pfn_u32 = unsafe { (*section.meta.add(pfn_u32 as usize)).next };
            }
        }

        Err(AllocError::NoMemory)
    }

    /// Finds a PFN within a free block that satisfies the alignment requirement.
    ///
    /// Returns `None` if no suitably aligned sub-block exists within the block.
    fn find_aligned_pfn_in_block(
        heap_start: usize,
        block_pfn: usize,
        block_order: usize,
        alloc_order: usize,
        align: usize,
        page_size: usize,
    ) -> Option<usize> {
        let subblock_pages = 1usize << alloc_order;
        let align_pages = align / page_size;
        let heap_page_offset = (heap_start / page_size) & (align_pages - 1);
        let offset = (align_pages - heap_page_offset) & (align_pages - 1);

        let candidate = if align_pages <= subblock_pages {
            if !is_aligned(heap_start, align) {
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

    /// Allocates pages whose physical address is below 4 GiB (DMA32 zone).
    ///
    /// Searches all sections for a free block whose physical address satisfies
    /// the alignment constraint and lies entirely below the DMA32 limit.
    pub fn alloc_frames_lowmem(&mut self, count: usize, align: usize) -> AllocResult<PhysAddr> {
        if count == 0 {
            return Err(AllocError::InvalidParam);
        }
        let align = if align == 0 { self.page_size } else { align };
        if !align.is_power_of_two() || align < self.page_size {
            return Err(AllocError::InvalidParam);
        }

        let order = count.next_power_of_two().trailing_zeros() as usize;
        if order > MAX_ORDER {
            return Err(AllocError::InvalidParam);
        }

        let page_size = self.page_size;
        let virt_phys_offset = self.virt_phys_offset;

        let mut section = self.sections_head;
        while !section.is_null() {
            if let Ok(vaddr) =
                unsafe { Self::alloc_lowmem_from_section(&mut *section, order, align, page_size) }
            {
                let paddr = vaddr - virt_phys_offset;
                let block_bytes = (1usize << order) * page_size;
                if paddr + block_bytes <= DMA32_LIMIT {
                    return Ok(PhysAddr::from(paddr));
                }
                // The allocation succeeded but is not in the DMA32 zone.
                // Free it and try the next candidate in the same section.
                unsafe {
                    let sec = &mut *section;
                    let pfn = (vaddr - sec.heap_start) / page_size;
                    let stored_order = (*sec.meta.add(pfn)).order as usize;
                    Self::dealloc_in_section(sec, pfn, stored_order);
                }
                // Do not advance to the next section; there may be other
                // candidates in this section's free lists at higher orders.
            } else {
                section = unsafe { (*section).next };
            }
        }

        Err(AllocError::NoMemory)
    }

    /// Attempts a low-memory allocation from a single section.
    ///
    /// Returns the virtual address of the allocated block without performing
    /// the DMA32 physical-address check (the caller is responsible for that).
    fn alloc_lowmem_from_section(
        section: &mut BuddySection,
        alloc_order: usize,
        align: usize,
        page_size: usize,
    ) -> AllocResult<usize> {
        for search_order in alloc_order..=MAX_ORDER {
            let mut pfn_u32 = section.free_lists[search_order];
            while pfn_u32 != PFN_NONE {
                let block_pfn = pfn_u32 as usize;
                let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    section.heap_start,
                    block_pfn,
                    search_order,
                    alloc_order,
                    align,
                    page_size,
                ) else {
                    pfn_u32 = unsafe { (*section.meta.add(pfn_u32 as usize)).next };
                    continue;
                };
                let vaddr = section.heap_start + target_pfn * page_size;
                unsafe {
                    free_list_remove(section.meta, &mut section.free_lists, pfn_u32, search_order);
                }

                let mut current_order = search_order;
                let mut current_pfn = block_pfn;
                while current_order > alloc_order {
                    current_order -= 1;
                    let left_pfn = current_pfn;
                    let right_pfn = current_pfn + (1 << current_order);
                    let (next_pfn, free_pfn) = if target_pfn >= right_pfn {
                        (right_pfn, left_pfn)
                    } else {
                        (left_pfn, right_pfn)
                    };
                    unsafe {
                        let bm = &mut *section.meta.add(free_pfn);
                        bm.flags = PageFlags::Free;
                        bm.order = current_order as u8;
                        free_list_push(
                            section.meta,
                            &mut section.free_lists,
                            free_pfn as u32,
                            current_order,
                        );
                    }
                    current_pfn = next_pfn;
                }

                unsafe {
                    let m = &mut *section.meta.add(current_pfn);
                    m.flags = PageFlags::Allocated;
                    m.order = alloc_order as u8;
                }
                section.free_pages -= 1 << alloc_order;
                return Ok(vaddr);
            }
        }

        Err(AllocError::NoMemory)
    }

    /// Performs buddy-merging deallocation within a single section.
    ///
    /// Starting from the given PFN and order, checks whether the buddy block
    /// is free and of the same order. If so, removes the buddy from its free
    /// list and merges into a block of the next order. Repeats until no more
    /// merging is possible, then pushes the resulting block onto its free list.
    fn dealloc_in_section(section: &mut BuddySection, mut pfn: usize, mut order: usize) {
        let freed_pages = 1usize << order;

        while order < MAX_ORDER {
            let buddy_pfn = pfn ^ (1 << order);
            if buddy_pfn >= section.max_pages {
                break;
            }
            let buddy = unsafe { &*section.meta.add(buddy_pfn) };
            if buddy.flags != PageFlags::Free || buddy.order as usize != order {
                break;
            }
            unsafe {
                free_list_remove(
                    section.meta,
                    &mut section.free_lists,
                    buddy_pfn as u32,
                    order,
                );
            }
            pfn = pfn.min(buddy_pfn);
            order += 1;
        }

        unsafe {
            let m = &mut *section.meta.add(pfn);
            m.flags = PageFlags::Free;
            m.order = order as u8;
            free_list_push(section.meta, &mut section.free_lists, pfn as u32, order);
        }
        section.free_pages += freed_pages;
    }

    /// Finds the section whose heap contains the given virtual address.
    fn find_section_by_addr(&self, addr: usize) -> Option<&BuddySection> {
        let mut section = self.sections_head;
        while !section.is_null() {
            let current = unsafe { &*section };
            if current.contains_heap_addr(addr) {
                return Some(current);
            }
            section = current.next;
        }
        None
    }

    /// Finds the section whose heap contains the given virtual address (mutable).
    fn find_section_by_addr_mut(&mut self, addr: usize) -> Option<&mut BuddySection> {
        let mut section = self.sections_head;
        while !section.is_null() {
            let current = unsafe { &mut *section };
            if current.contains_heap_addr(addr) {
                return Some(current);
            }
            section = current.next;
        }
        None
    }
}
