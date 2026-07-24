use core::mem;
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    env, eprintln,
    panic::AssertUnwindSafe,
    ptr::NonNull,
    time::{SystemTime, UNIX_EPOCH},
    vec,
    vec::Vec,
};

use memory_addr::{MemoryAddr, PhysAddrRange, pa};

use crate::{
    BuddyAllocator, BuddyError, BuddySection, MAX_ORDER, page_count_to_order_ceiling,
    page_count_to_order_floor,
};

const PAGE_SHIFTS: [usize; 2] = [4, 5];
const RANDOM_PHASES: usize = 3;
const RANDOM_OPS: usize = 180;

fn run_for_page_sizes(mut case: impl FnMut(usize)) {
    for page_size_shift in PAGE_SHIFTS {
        case(page_size_shift);
    }
}

const fn page_size(page_size_shift: usize) -> usize {
    1usize << page_size_shift
}

fn align_up_usize(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

struct SectionMemory {
    ptr: NonNull<u8>,
    layout: Layout,
    start: usize,
    bytes: usize,
}

impl SectionMemory {
    fn new(page_size_shift: usize, total_pages: usize, misalign: usize) -> Self {
        let ps = page_size(page_size_shift);
        let bytes = total_pages << page_size_shift;
        let layout =
            Layout::from_size_align(bytes + ps + misalign + mem::align_of::<usize>(), ps).unwrap();
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("test allocation failed");
        let base = ptr.as_ptr() as usize + misalign;
        let start = align_up_usize(base, ps);

        Self {
            ptr,
            layout,
            start,
            bytes,
        }
    }

    fn range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(pa!(self.start), self.bytes)
    }
}

impl Drop for SectionMemory {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

fn total_pages_for_heap_at_least(page_size_shift: usize, min_heap_pages: usize) -> usize {
    let mut total_pages = min_heap_pages + 1;
    while total_pages <= BuddyAllocator::MAX_HEAP_PAGES_IN_SECTION {
        if let Some(layout) = BuddySection::layout_for_section(total_pages, page_size_shift)
            && layout.heap_pages >= min_heap_pages
        {
            return total_pages;
        }
        total_pages += 1;
    }
    panic!("no valid section layout for at least {min_heap_pages} heap pages");
}

fn huge_total_pages(page_size_shift: usize) -> usize {
    total_pages_for_heap_at_least(page_size_shift, (1usize << MAX_ORDER) * 2 + 1)
}

struct AllocatorFixture {
    allocator: BuddyAllocator,
    sections: Vec<SectionMemory>,
    page_size_shift: usize,
}

impl AllocatorFixture {
    fn empty(page_size_shift: usize) -> Self {
        let mut allocator = BuddyAllocator::new();
        unsafe { allocator.init(page_size_shift, 0).unwrap() };
        Self {
            allocator,
            sections: Vec::new(),
            page_size_shift,
        }
    }

    fn with_huge_sections(page_size_shift: usize) -> Self {
        let mut fixture = Self::empty(page_size_shift);
        for (index, total_pages) in [
            total_pages_for_heap_at_least(page_size_shift, 3),
            total_pages_for_heap_at_least(page_size_shift, 257),
            total_pages_for_heap_at_least(page_size_shift, 409),
            huge_total_pages(page_size_shift),
        ]
        .into_iter()
        .enumerate()
        {
            fixture.add_section(total_pages, index * 7 + 1);
        }
        fixture
    }

    fn with_standard_sections(page_size_shift: usize) -> Self {
        let mut fixture = Self::empty(page_size_shift);
        for (index, total_pages) in [
            total_pages_for_heap_at_least(page_size_shift, 64),
            total_pages_for_heap_at_least(page_size_shift, 129),
            total_pages_for_heap_at_least(page_size_shift, 257),
        ]
        .into_iter()
        .enumerate()
        {
            fixture.add_section(total_pages, index * 11 + 1);
        }
        fixture
    }

    fn add_section(&mut self, total_pages: usize, misalign: usize) {
        let section = SectionMemory::new(self.page_size_shift, total_pages, misalign);
        unsafe { self.allocator.add_section(section.range()).unwrap() };
        self.sections.push(section);
    }

    fn model(&self) -> ShadowModel {
        ShadowModel::from_allocator(&self.allocator, self.page_size_shift)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiFamily {
    Block,
    Frames,
    Frame,
    BlocksAt,
    FramesAt,
}

#[derive(Debug, Clone)]
struct AllocationRecord {
    addr: usize,
    pages: usize,
    order: usize,
    family: ApiFamily,
}

#[derive(Debug)]
struct ModelBlock {
    start: usize,
    order: usize,
    free: bool,
}

#[derive(Debug)]
struct ModelSection {
    heap_start: usize,
    heap_base_pfn: usize,
    heap_pages: usize,
    free: Vec<bool>,
    blocks: Vec<ModelBlock>,
}

#[derive(Debug)]
struct ShadowModel {
    page_size_shift: usize,
    sections: Vec<ModelSection>,
    allocations: Vec<AllocationRecord>,
}

impl ShadowModel {
    fn from_allocator(allocator: &BuddyAllocator, page_size_shift: usize) -> Self {
        let sections = allocator
            .section_iter()
            .map(|section| {
                let heap_start = section.heap_region.start.as_usize();
                let heap_base_pfn = heap_start >> page_size_shift;
                let heap_pages = section.stats.heap_pages;
                ModelSection {
                    heap_start,
                    heap_base_pfn,
                    heap_pages,
                    free: vec![true; heap_pages],
                    blocks: Self::initial_blocks(heap_base_pfn, heap_pages),
                }
            })
            .collect();

        Self {
            page_size_shift,
            sections,
            allocations: Vec::new(),
        }
    }

    fn initial_blocks(heap_base_pfn: usize, heap_pages: usize) -> Vec<ModelBlock> {
        let mut blocks = Vec::new();
        let mut page = 0usize;
        while page < heap_pages {
            let order = Self::max_order_for_pfn_in_heap(heap_base_pfn + page, page, heap_pages);
            blocks.push(ModelBlock {
                start: page,
                order,
                free: true,
            });
            page += 1usize << order;
        }
        blocks
    }

    fn max_order_for_pfn_in_heap(pfn: usize, page: usize, heap_pages: usize) -> usize {
        let mut order = (pfn.trailing_zeros() as usize).min(MAX_ORDER);
        while order > 0 && page + (1usize << order) > heap_pages {
            order -= 1;
        }
        order
    }

    const fn page_size(&self) -> usize {
        page_size(self.page_size_shift)
    }

    fn locate_addr(&self, addr: usize) -> Option<(usize, usize)> {
        let ps = self.page_size();
        self.sections
            .iter()
            .enumerate()
            .find_map(|(index, section)| {
                let section_end = section.heap_start + section.heap_pages * ps;
                if (section.heap_start..section_end).contains(&addr) {
                    Some((index, (addr - section.heap_start) / ps))
                } else {
                    None
                }
            })
    }

    fn locate_range(&self, addr: usize, pages: usize) -> Option<(usize, usize)> {
        if pages == 0
            || self.range_end(addr, pages).is_none()
            || !addr.is_multiple_of(self.page_size())
        {
            return None;
        }
        self.locate_aligned_range(addr, pages)
    }

    fn range_end(&self, addr: usize, pages: usize) -> Option<usize> {
        addr.checked_add(pages.checked_mul(self.page_size())?)
    }

    fn range_is_page_aligned(&self, addr: usize, pages: usize) -> bool {
        self.range_end(addr, pages).is_some_and(|end| {
            addr.is_multiple_of(self.page_size()) && end.is_multiple_of(self.page_size())
        })
    }

    fn range_fits_section(&self, section_index: usize, addr: usize, pages: usize) -> bool {
        let section = &self.sections[section_index];
        let section_end = section.heap_start + section.heap_pages * self.page_size();
        self.range_end(addr, pages)
            .is_some_and(|range_end| range_end <= section_end)
    }

    fn locate_aligned_range(&self, addr: usize, pages: usize) -> Option<(usize, usize)> {
        let (section_index, page_index) = self.locate_addr(addr)?;
        if self.range_fits_section(section_index, addr, pages) {
            Some((section_index, page_index))
        } else {
            None
        }
    }

    fn range_is_free(&self, section_index: usize, page_index: usize, pages: usize) -> bool {
        self.sections[section_index].free[page_index..page_index + pages]
            .iter()
            .all(|&free| free)
    }

    fn range_is_allocated(&self, section_index: usize, page_index: usize, pages: usize) -> bool {
        self.sections[section_index].free[page_index..page_index + pages]
            .iter()
            .all(|&free| !free)
    }

    fn block_index_containing(&self, section_index: usize, page_index: usize) -> usize {
        self.sections[section_index]
            .blocks
            .iter()
            .position(|block| {
                let block_end = block.start + (1usize << block.order);
                (block.start..block_end).contains(&page_index)
            })
            .unwrap()
    }

    fn split_block_to(
        &mut self,
        section_index: usize,
        block_index: usize,
        target_start: usize,
        target_order: usize,
    ) -> usize {
        let block = self.sections[section_index].blocks.remove(block_index);
        assert!(block.free);
        assert!(block.start <= target_start);
        assert!(target_start + (1usize << target_order) <= block.start + (1usize << block.order));

        fn split_pieces(
            start: usize,
            order: usize,
            target_start: usize,
            target_order: usize,
            pieces: &mut Vec<ModelBlock>,
        ) -> usize {
            if order == target_order {
                assert_eq!(start, target_start);
                let target_index = pieces.len();
                pieces.push(ModelBlock {
                    start,
                    order,
                    free: true,
                });
                return target_index;
            }

            let child_order = order - 1;
            let right_start = start + (1usize << child_order);
            if target_start < right_start {
                let target_index =
                    split_pieces(start, child_order, target_start, target_order, pieces);
                pieces.push(ModelBlock {
                    start: right_start,
                    order: child_order,
                    free: true,
                });
                target_index
            } else {
                pieces.push(ModelBlock {
                    start,
                    order: child_order,
                    free: true,
                });
                split_pieces(right_start, child_order, target_start, target_order, pieces)
            }
        }

        let mut pieces = Vec::new();
        let target_offset = split_pieces(
            block.start,
            block.order,
            target_start,
            target_order,
            &mut pieces,
        );
        for (offset, piece) in pieces.into_iter().enumerate() {
            self.sections[section_index]
                .blocks
                .insert(block_index + offset, piece);
        }
        block_index + target_offset
    }

    fn mark_block_allocated(&mut self, section_index: usize, page_index: usize, order: usize) {
        let block_index = self.block_index_containing(section_index, page_index);
        let block_index = if self.sections[section_index].blocks[block_index].start == page_index
            && self.sections[section_index].blocks[block_index].order == order
        {
            block_index
        } else {
            self.split_block_to(section_index, block_index, page_index, order)
        };
        let block = &mut self.sections[section_index].blocks[block_index];
        assert!(block.free);
        assert_eq!(block.start, page_index);
        assert_eq!(block.order, order);
        block.free = false;
    }

    fn buddy_start(&self, section_index: usize, start: usize, order: usize) -> Option<usize> {
        let section = &self.sections[section_index];
        let buddy_pfn = (section.heap_base_pfn + start) ^ (1usize << order);
        if buddy_pfn < section.heap_base_pfn {
            return None;
        }
        let buddy_start = buddy_pfn - section.heap_base_pfn;
        if buddy_start + (1usize << order) <= section.heap_pages {
            Some(buddy_start)
        } else {
            None
        }
    }

    fn mark_block_free(&mut self, section_index: usize, page_index: usize, order: usize) {
        let mut block_index = self.block_index_containing(section_index, page_index);
        {
            let block = &mut self.sections[section_index].blocks[block_index];
            assert!(!block.free);
            assert_eq!(block.start, page_index);
            assert_eq!(block.order, order);
            block.free = true;
        }

        loop {
            let block = &self.sections[section_index].blocks[block_index];
            if block.order >= MAX_ORDER {
                break;
            }
            let Some(buddy_start) = self.buddy_start(section_index, block.start, block.order)
            else {
                break;
            };
            let Some(buddy_index) = self.sections[section_index]
                .blocks
                .iter()
                .position(|buddy| {
                    buddy.start == buddy_start && buddy.order == block.order && buddy.free
                })
            else {
                break;
            };

            let new_start = block.start.min(buddy_start);
            let new_order = block.order + 1;
            for index in [block_index.max(buddy_index), block_index.min(buddy_index)] {
                self.sections[section_index].blocks.remove(index);
            }
            let insert_index = self.sections[section_index]
                .blocks
                .iter()
                .position(|block| block.start > new_start)
                .unwrap_or(self.sections[section_index].blocks.len());
            self.sections[section_index].blocks.insert(
                insert_index,
                ModelBlock {
                    start: new_start,
                    order: new_order,
                    free: true,
                },
            );
            block_index = insert_index;
        }
    }

    fn mark_allocated_range(&mut self, addr: usize, pages: usize) {
        let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
        let end_page = page_index + pages;
        let mut current_page = page_index;
        while current_page < end_page {
            let block_index = self.block_index_containing(section_index, current_page);
            let block = &self.sections[section_index].blocks[block_index];
            assert!(block.free);
            let block_end = block.start + (1usize << block.order);
            let order = if block.start == current_page && block_end <= end_page {
                block.order
            } else if block.start == current_page {
                page_count_to_order_floor(end_page - current_page).unwrap()
            } else {
                Self::max_order_for_pfn_in_heap(
                    self.sections[section_index].heap_base_pfn + current_page,
                    current_page,
                    block_end.min(end_page),
                )
            };
            self.mark_block_allocated(section_index, current_page, order);
            current_page += 1usize << order;
        }
    }

    fn range_matches_allocated_blocks(
        &self,
        section_index: usize,
        page_index: usize,
        pages: usize,
    ) -> Result<(), BuddyError> {
        let end_page = page_index + pages;
        let mut current_page = page_index;
        while current_page < end_page {
            let block = &self.sections[section_index].blocks
                [self.block_index_containing(section_index, current_page)];
            if block.start != current_page || block.start + (1usize << block.order) > end_page {
                return Err(BuddyError::OrderMismatch);
            }
            if block.free {
                return Err(BuddyError::NotAllocated);
            }
            current_page += 1usize << block.order;
        }
        Ok(())
    }

    fn mark_free_range(&mut self, addr: usize, pages: usize) {
        let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
        let end_page = page_index + pages;
        let mut blocks = Vec::new();
        let mut current_page = page_index;
        while current_page < end_page {
            let block = &self.sections[section_index].blocks
                [self.block_index_containing(section_index, current_page)];
            assert_eq!(block.start, current_page);
            assert!(!block.free);
            blocks.push((block.start, block.order));
            current_page += 1usize << block.order;
        }
        for (start, order) in blocks {
            self.mark_block_free(section_index, start, order);
        }
    }

    fn mark_allocated(&mut self, addr: usize, pages: usize, order: usize, family: ApiFamily) {
        if pages > 0 {
            let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
            assert!(self.range_is_free(section_index, page_index, pages));
            if matches!(family, ApiFamily::BlocksAt | ApiFamily::FramesAt) {
                self.mark_allocated_range(addr, pages);
            } else {
                self.mark_block_allocated(section_index, page_index, order);
            }
            for free in &mut self.sections[section_index].free[page_index..page_index + pages] {
                *free = false;
            }
        }
        self.allocations.push(AllocationRecord {
            addr,
            pages,
            order,
            family,
        });
    }

    fn mark_free(&mut self, addr: usize, pages: usize) {
        if pages == 0 {
            return;
        }
        let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
        assert!(self.range_is_allocated(section_index, page_index, pages));
        self.mark_free_range(addr, pages);
        for free in &mut self.sections[section_index].free[page_index..page_index + pages] {
            *free = true;
        }
    }

    fn expected_alloc_at_error(&self, addr: usize, pages: usize) -> Option<BuddyError> {
        if pages == 0 {
            return Some(BuddyError::InvalidPageCount);
        }
        let Some((section_index, page_index)) = self.locate_addr(addr) else {
            return Some(BuddyError::NotInHeap);
        };
        if !self.range_fits_section(section_index, addr, pages) {
            return Some(BuddyError::NotInHeap);
        }
        if !self.range_is_page_aligned(addr, pages) {
            return Some(BuddyError::NotAligned);
        }
        if self.range_is_free(section_index, page_index, pages) {
            None
        } else {
            Some(BuddyError::AlreadyAllocated)
        }
    }

    fn mark_records_free_in_range(&mut self, addr: usize, pages: usize) {
        let ps = self.page_size();
        let end = self.range_end(addr, pages).unwrap();
        self.mark_free(addr, pages);
        self.allocations.retain(|record| {
            let record_end = record.addr + record.pages * ps;
            !(addr <= record.addr && record_end <= end)
        });
    }

    fn expected_dealloc_at_error(&self, addr: usize, pages: usize) -> Option<BuddyError> {
        if pages == 0 {
            return Some(BuddyError::InvalidPageCount);
        }
        let Some((section_index, page_index)) = self.locate_addr(addr) else {
            return Some(BuddyError::NotInHeap);
        };
        if !self.range_fits_section(section_index, addr, pages) {
            return Some(BuddyError::NotInHeap);
        }
        if !self.range_is_page_aligned(addr, pages) {
            return Some(BuddyError::NotAligned);
        }
        self.range_matches_allocated_blocks(section_index, page_index, pages)
            .err()
    }

    fn alloc_block_checked(
        &mut self,
        allocator: &mut BuddyAllocator,
        order: usize,
        align: usize,
        family: ApiFamily,
    ) -> Result<usize, BuddyError> {
        let result = match family {
            ApiFamily::Frame => allocator.alloc_frame(),
            ApiFamily::Frames => allocator.alloc_frames(1usize << order, align),
            _ => allocator.alloc_block(order, align),
        };
        match result {
            Ok(addr) => {
                let addr = addr.as_usize();
                let pages = 1usize << order;
                assert_eq!(addr % self.page_size(), 0);
                assert_eq!(addr % align, 0);
                let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
                assert!(self.range_is_free(section_index, page_index, pages));
                self.mark_allocated(addr, pages, order, family);
                Ok(addr)
            }
            Err(error) => Err(error),
        }
    }

    fn alloc_frames_checked(
        &mut self,
        allocator: &mut BuddyAllocator,
        count: usize,
        align: usize,
    ) -> Result<usize, BuddyError> {
        let result = allocator.alloc_frames(count, align);
        match result {
            Ok(addr) => {
                let order = page_count_to_order_ceiling(count).unwrap_or(0);
                let pages = if count == 0 { 0 } else { 1usize << order };
                let addr = addr.as_usize();
                assert_eq!(addr % self.page_size(), 0);
                assert_eq!(addr % align, 0);
                if pages > 0 {
                    let (section_index, page_index) = self.locate_range(addr, pages).unwrap();
                    assert!(self.range_is_free(section_index, page_index, pages));
                }
                self.mark_allocated(addr, pages, order, ApiFamily::Frames);
                Ok(addr)
            }
            Err(error) => Err(error),
        }
    }

    fn dealloc_record_checked(
        &mut self,
        allocator: &mut BuddyAllocator,
        index: usize,
    ) -> Result<(), BuddyError> {
        let record = self.allocations[index].clone();
        let result = match record.family {
            ApiFamily::Block => allocator.dealloc_block(pa!(record.addr), record.order),
            ApiFamily::Frames => allocator.dealloc_frames(pa!(record.addr), record.pages),
            ApiFamily::Frame => allocator.dealloc_frame(pa!(record.addr)),
            ApiFamily::BlocksAt => allocator.dealloc_blocks_at(pa!(record.addr), record.pages),
            ApiFamily::FramesAt => allocator.dealloc_frames_at(pa!(record.addr), record.pages),
        };
        if result.is_ok() {
            self.mark_free(record.addr, record.pages);
            self.allocations.swap_remove(index);
        }
        result
    }

    fn alloc_blocks_at_checked(
        &mut self,
        allocator: &mut BuddyAllocator,
        addr: usize,
        pages: usize,
        family: ApiFamily,
    ) {
        let expected_error = self.expected_alloc_at_error(addr, pages);
        let result = match family {
            ApiFamily::FramesAt => allocator.alloc_frames_at(pa!(addr), pages),
            _ => allocator.alloc_blocks_at(pa!(addr), pages),
        };
        match (expected_error, result) {
            (None, Ok(())) => {
                let order = page_count_to_order_floor(pages).unwrap_or(0);
                self.mark_allocated(addr, pages, order, family);
            }
            (Some(expected), Err(actual)) => assert_eq!(actual, expected),
            other => panic!("alloc_at mismatch addr={addr:#x} pages={pages}: {other:?}"),
        }
        self.assert_stats_match(allocator);
    }

    fn dealloc_blocks_at_checked(
        &mut self,
        allocator: &mut BuddyAllocator,
        addr: usize,
        pages: usize,
        family: ApiFamily,
    ) {
        let expected_error = self.expected_dealloc_at_error(addr, pages);
        let result = match family {
            ApiFamily::FramesAt => allocator.dealloc_frames_at(pa!(addr), pages),
            _ => allocator.dealloc_blocks_at(pa!(addr), pages),
        };
        match (expected_error, result) {
            (None, Ok(())) => self.mark_records_free_in_range(addr, pages),
            (
                Some(BuddyError::NotAllocated | BuddyError::OrderMismatch),
                Err(BuddyError::NotAllocated | BuddyError::OrderMismatch),
            ) => {}
            (Some(expected), Err(actual)) => assert_eq!(actual, expected),
            other => panic!("dealloc_at mismatch addr={addr:#x} pages={pages}: {other:?}"),
        }
        self.assert_stats_match(allocator);
    }

    fn assert_stats_match(&self, allocator: &BuddyAllocator) {
        let mut expected_heap = 0usize;
        let mut expected_free = 0usize;
        for section in &self.sections {
            expected_heap += section.heap_pages;
            expected_free += section.free.iter().filter(|&&free| free).count();
        }

        let stats = allocator.stats();
        assert_eq!(stats.heap_pages, expected_heap);
        assert_eq!(stats.free_pages, expected_free);

        for (index, section) in self.sections.iter().enumerate() {
            let stats = allocator.section_stats(index).unwrap();
            assert_eq!(stats.heap_pages, section.heap_pages);
            assert_eq!(
                stats.free_pages,
                section.free.iter().filter(|&&free| free).count()
            );
        }
        assert_eq!(allocator.section_stats(self.sections.len()).is_none(), true);
    }

    fn assert_pages_match(&self, allocator: &BuddyAllocator) {
        let ps = self.page_size();
        for section in &self.sections {
            for page in 0..section.heap_pages {
                let addr = section.heap_start + page * ps;
                let expected_allocated = !section.free[page];
                assert_eq!(allocator.is_allocated(pa!(addr)), Ok(expected_allocated));
                assert_eq!(
                    allocator.is_allocated(pa!(addr + ps - 1)),
                    Ok(expected_allocated)
                );
            }
        }
    }
}

#[test]
fn initialization_and_section_boundaries() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut allocator = BuddyAllocator::new();
        assert!(!allocator.is_initialized());
        assert_eq!(
            allocator.ensure_initialized(),
            Err(BuddyError::NotInitialized)
        );
        assert_eq!(
            unsafe { allocator.init(0, 0) },
            Err(BuddyError::InvalidPageSize)
        );
        assert_eq!(allocator.alloc_frame(), Err(BuddyError::NoMemory));

        unsafe { allocator.init(page_size_shift, 0).unwrap() };
        assert!(allocator.is_initialized());
        assert_eq!(allocator.ensure_initialized(), Ok(()));
        assert!(allocator.find_section_by_addr(pa!(0)).is_none());

        let fixture = AllocatorFixture::with_huge_sections(page_size_shift);
        assert_eq!(fixture.allocator.section_count(), 4);
        let stats = fixture.allocator.stats();
        assert!(stats.heap_pages > (1usize << MAX_ORDER) * 2);
        assert_eq!(stats.free_pages, stats.heap_pages);

        for (index, section) in fixture.allocator.section_iter().enumerate() {
            let section_stats = fixture.allocator.section_stats(index).unwrap();
            assert_eq!(section.stats().heap_pages, section_stats.heap_pages);
            assert_eq!(section.stats().free_pages, section_stats.free_pages);
            assert_eq!(section_stats.free_pages, section_stats.heap_pages);
            assert!(section.region.contains_range(section.heap_region));
            assert!(section.heap_region.start.is_aligned(page_size));
            assert!(section.heap_region.end.is_aligned(page_size));
            assert!(core::ptr::eq(
                fixture
                    .allocator
                    .find_section_by_addr(section.heap_region.start)
                    .unwrap(),
                section,
            ));
        }
    });
}

#[test]
fn add_section_layout_and_overlap_errors() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut allocator = BuddyAllocator::new();
        unsafe { allocator.init(page_size_shift, 0).unwrap() };

        let too_small = SectionMemory::new(page_size_shift, 1, 0);
        assert_eq!(
            unsafe { allocator.add_section(too_small.range()) },
            Err(BuddyError::SectionTooSmall)
        );

        let valid = SectionMemory::new(
            page_size_shift,
            total_pages_for_heap_at_least(page_size_shift, 32),
            0,
        );
        let valid_range = valid.range();
        let unaligned =
            PhysAddrRange::from_start_size(valid_range.start + 1, valid_range.size() - 1);
        assert_eq!(
            unsafe { allocator.add_section(unaligned) },
            Err(BuddyError::NotAligned)
        );

        assert_eq!(
            allocator.check_metadata_overlap(valid_range, valid_range),
            Ok(true)
        );
        unsafe { allocator.add_section(valid_range).unwrap() };
        assert_eq!(
            unsafe { allocator.add_section(valid_range) },
            Err(BuddyError::SectionOverlap)
        );

        let layout = BuddySection::layout_for_section(valid.bytes / page_size, page_size_shift)
            .expect("valid section should have a layout");
        let heap_start = valid_range.start + layout.metadata_pages * page_size;
        assert_eq!(
            allocator.check_metadata_overlap(
                valid_range,
                PhysAddrRange::new(heap_start, heap_start + page_size),
            ),
            Ok(false)
        );
    });
}

#[test]
fn alloc_block_and_frame_boundaries() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut fixture = AllocatorFixture::with_huge_sections(page_size_shift);
        let mut model = fixture.model();

        assert_eq!(
            model.alloc_block_checked(
                &mut fixture.allocator,
                MAX_ORDER + 1,
                page_size,
                ApiFamily::Block,
            ),
            Err(BuddyError::InvalidOrder)
        );
        for align in [0, page_size / 2, page_size + 1] {
            assert_eq!(
                model.alloc_block_checked(&mut fixture.allocator, 0, align, ApiFamily::Block),
                Err(BuddyError::InvalidAlignment)
            );
        }
        model.assert_stats_match(&fixture.allocator);

        for order in 0..=MAX_ORDER {
            let result = model.alloc_block_checked(
                &mut fixture.allocator,
                order,
                page_size,
                ApiFamily::Block,
            );
            assert!(result.is_ok() || result == Err(BuddyError::NoMemory));
            model.assert_stats_match(&fixture.allocator);
        }

        let frame = model
            .alloc_block_checked(&mut fixture.allocator, 0, page_size, ApiFamily::Frame)
            .unwrap();
        let frame_index = model
            .allocations
            .iter()
            .position(|record| record.addr == frame)
            .unwrap();
        assert_eq!(
            model.dealloc_record_checked(&mut fixture.allocator, frame_index),
            Ok(())
        );

        for count in [
            0,
            1,
            2,
            3,
            4,
            7,
            8,
            9,
            1usize << MAX_ORDER,
            (1usize << MAX_ORDER) + 1,
        ] {
            let result = model.alloc_frames_checked(&mut fixture.allocator, count, page_size);
            if count == 0 || count > (1usize << MAX_ORDER) {
                assert_eq!(result, Err(BuddyError::InvalidPageCount));
            } else {
                assert!(result.is_ok() || result == Err(BuddyError::NoMemory));
            }
            model.assert_stats_match(&fixture.allocator);
        }

        let before = fixture.allocator.stats();
        let rounded = fixture.allocator.alloc_frames(3, page_size).unwrap();
        let after_alloc = fixture.allocator.stats();
        assert_eq!(after_alloc.used_pages(), before.used_pages() + 4);
        assert_eq!(after_alloc.free_pages(), before.free_pages() - 4);
        fixture.allocator.dealloc_frames(rounded, 3).unwrap();
        let after_dealloc = fixture.allocator.stats();
        assert_eq!(after_dealloc.total_pages(), before.total_pages());
        assert_eq!(after_dealloc.meta_pages(), before.meta_pages());
        assert_eq!(after_dealloc.heap_pages(), before.heap_pages());
        assert_eq!(after_dealloc.used_pages(), before.used_pages());
        assert_eq!(after_dealloc.free_pages(), before.free_pages());
        assert_eq!(after_dealloc.unused_pages(), before.unused_pages());
    });
}

#[test]
fn alloc_at_non_natural_boundaries() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut fixture = AllocatorFixture::with_huge_sections(page_size_shift);
        let mut model = fixture.model();
        let huge = model
            .sections
            .iter()
            .max_by_key(|section| section.heap_pages)
            .unwrap()
            .heap_start;
        let ranges = [
            (huge + page_size, 3),
            (huge + 5 * page_size, 17),
            (huge + ((1usize << 10) - 3) * page_size, 19),
            (huge + ((1usize << MAX_ORDER) - 5) * page_size, 11),
        ];

        for (addr, pages) in ranges {
            model.alloc_blocks_at_checked(&mut fixture.allocator, addr, pages, ApiFamily::BlocksAt);
        }
        model.assert_stats_match(&fixture.allocator);

        for (addr, pages) in [ranges[1], ranges[3], ranges[0], ranges[2]] {
            model.dealloc_blocks_at_checked(
                &mut fixture.allocator,
                addr,
                pages,
                ApiFamily::BlocksAt,
            );
        }
        model.assert_stats_match(&fixture.allocator);
    });
}

#[test]
fn dealloc_mismatch_and_error_paths() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut fixture = AllocatorFixture::with_standard_sections(page_size_shift);
        let mut model = fixture.model();
        let heap_start = model.sections[0].heap_start;
        let heap_pages = model.sections[0].heap_pages;

        model.alloc_blocks_at_checked(&mut fixture.allocator, heap_start, 0, ApiFamily::BlocksAt);
        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + 1,
            1,
            ApiFamily::BlocksAt,
        );
        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start - page_size,
            2,
            ApiFamily::BlocksAt,
        );
        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + (heap_pages - 1) * page_size,
            2,
            ApiFamily::BlocksAt,
        );

        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + page_size,
            5,
            ApiFamily::BlocksAt,
        );
        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + 2 * page_size,
            2,
            ApiFamily::BlocksAt,
        );
        model.dealloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + 2 * page_size,
            1,
            ApiFamily::BlocksAt,
        );
        model.dealloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + page_size,
            5,
            ApiFamily::BlocksAt,
        );
        model.dealloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + page_size,
            5,
            ApiFamily::BlocksAt,
        );

        model.alloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + 8 * page_size,
            4,
            ApiFamily::FramesAt,
        );
        model.dealloc_blocks_at_checked(
            &mut fixture.allocator,
            heap_start + 8 * page_size,
            4,
            ApiFamily::FramesAt,
        );
    });
}

#[test]
fn exhaustion_and_reuse_patterns() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = page_size(page_size_shift);
        let mut fixture = AllocatorFixture::with_standard_sections(page_size_shift);
        let mut model = fixture.model();

        loop {
            let order = model.allocations.len() % 4;
            match model.alloc_block_checked(
                &mut fixture.allocator,
                order,
                page_size,
                ApiFamily::Block,
            ) {
                Ok(_) => model.assert_stats_match(&fixture.allocator),
                Err(BuddyError::NoMemory) => break,
                Err(error) => panic!("unexpected exhaustion error: {error:?}"),
            }
        }

        let mut index = 0;
        while index < model.allocations.len() {
            model
                .dealloc_record_checked(&mut fixture.allocator, index)
                .unwrap();
            index += 2;
        }

        for order in [4, 5, 6, 7, 5, 4] {
            let result = model.alloc_block_checked(
                &mut fixture.allocator,
                order,
                page_size,
                ApiFamily::Block,
            );
            assert!(result.is_ok() || result == Err(BuddyError::NoMemory));
            model.assert_stats_match(&fixture.allocator);
        }

        while !model.allocations.is_empty() {
            let index = model.allocations.len() - 1;
            model
                .dealloc_record_checked(&mut fixture.allocator, index)
                .unwrap();
        }
        model.assert_stats_match(&fixture.allocator);
        model.assert_pages_match(&fixture.allocator);
    });
}

#[derive(Debug, Clone, Copy)]
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn usize(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            self.next_u64() as usize % upper
        }
    }

    fn bool(&mut self, numerator: usize, denominator: usize) -> bool {
        self.usize(denominator) < numerator
    }
}

#[derive(Debug, Clone)]
enum Operation {
    AllocBlock {
        order: usize,
        align: usize,
    },
    AllocFrame,
    AllocFrames {
        count: usize,
        align: usize,
    },
    Dealloc {
        index: usize,
    },
    AllocAt {
        addr: usize,
        pages: usize,
        family: ApiFamily,
    },
    DeallocAt {
        addr: usize,
        pages: usize,
        family: ApiFamily,
    },
}

fn hash_name(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x1000_0000_01b3)
    })
}

fn test_seed(test_name: &str, page_size_shift: usize) -> u64 {
    if let Ok(seed) = env::var("EXBUDDY_TEST_SEED") {
        return seed.parse().expect("EXBUDDY_TEST_SEED must be u64");
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    nanos ^ ((page_size_shift as u64) << 32) ^ hash_name(test_name)
}

fn biased_count(rng: &mut Rng) -> usize {
    match rng.usize(12) {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 7,
        6 => 8,
        7 => 15,
        8 => 16,
        _ => rng.usize(96) + 1,
    }
}

fn biased_addr(model: &ShadowModel, rng: &mut Rng) -> usize {
    let section_index = rng.usize(model.sections.len());
    let section = &model.sections[section_index];
    let page = match rng.usize(6) {
        0 => 0,
        1 => section.heap_pages.saturating_sub(1),
        2 if !model.allocations.is_empty() => {
            let record = &model.allocations[rng.usize(model.allocations.len())];
            if (section.heap_start..section.heap_start + section.heap_pages * model.page_size())
                .contains(&record.addr)
            {
                (record.addr - section.heap_start) / model.page_size()
            } else {
                rng.usize(section.heap_pages)
            }
        }
        _ => rng.usize(section.heap_pages),
    };
    section.heap_start + page * model.page_size()
}

fn random_block_operation(model: &ShadowModel, rng: &mut Rng) -> Operation {
    if !model.allocations.is_empty() && rng.bool(1, 2) {
        return Operation::Dealloc {
            index: rng.usize(model.allocations.len()),
        };
    }
    let page_size = model.page_size();
    match rng.usize(4) {
        0 => Operation::AllocFrame,
        1 => Operation::AllocFrames {
            count: biased_count(rng),
            align: [page_size / 2, page_size, page_size * 2, page_size * 3][rng.usize(4)],
        },
        _ => Operation::AllocBlock {
            order: rng.usize(MAX_ORDER + 2),
            align: [page_size / 2, page_size, page_size * 2, page_size * 3][rng.usize(4)],
        },
    }
}

fn random_at_operation(model: &ShadowModel, rng: &mut Rng) -> Operation {
    let family = if rng.bool(1, 2) {
        ApiFamily::BlocksAt
    } else {
        ApiFamily::FramesAt
    };
    if !model.allocations.is_empty() && rng.bool(1, 2) {
        let record = &model.allocations[rng.usize(model.allocations.len())];
        if rng.bool(3, 4) {
            return Operation::DeallocAt {
                addr: record.addr,
                pages: record.pages,
                family: record.family,
            };
        }
        return Operation::DeallocAt {
            addr: record.addr + model.page_size(),
            pages: record.pages,
            family,
        };
    }
    let mut addr = biased_addr(model, rng);
    if rng.bool(1, 5) {
        addr += 1;
    }
    Operation::AllocAt {
        addr,
        pages: biased_count(rng),
        family,
    }
}

fn run_operation(model: &mut ShadowModel, allocator: &mut BuddyAllocator, operation: Operation) {
    match operation {
        Operation::AllocBlock { order, align } => {
            let _ = model.alloc_block_checked(allocator, order, align, ApiFamily::Block);
        }
        Operation::AllocFrame => {
            let _ = model.alloc_block_checked(allocator, 0, model.page_size(), ApiFamily::Frame);
        }
        Operation::AllocFrames { count, align } => {
            let _ = model.alloc_frames_checked(allocator, count, align);
        }
        Operation::Dealloc { index } => {
            let _ = model.dealloc_record_checked(allocator, index);
        }
        Operation::AllocAt {
            addr,
            pages,
            family,
        } => {
            model.alloc_blocks_at_checked(allocator, addr, pages, family);
        }
        Operation::DeallocAt {
            addr,
            pages,
            family,
        } => {
            model.dealloc_blocks_at_checked(allocator, addr, pages, family);
        }
    }
    model.assert_stats_match(allocator);
}

fn run_random_test(
    test_name: &str,
    page_size_shift: usize,
    operation: impl Fn(&ShadowModel, &mut Rng) -> Operation,
) {
    let seed = test_seed(test_name, page_size_shift);
    let mut rng = Rng::new(seed);
    let mut fixture = AllocatorFixture::with_standard_sections(page_size_shift);
    let mut model = fixture.model();

    for phase in 0..RANDOM_PHASES {
        for index in 0..RANDOM_OPS {
            let op = operation(&model, &mut rng);
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                run_operation(&mut model, &mut fixture.allocator, op.clone());
            }));
            if let Err(payload) = result {
                eprintln!(
                    "{test_name} failed seed={seed} page_size_shift={page_size_shift} phase={phase} index={index} op={op:?}"
                );
                std::panic::resume_unwind(payload);
            }
        }
        model.assert_pages_match(&fixture.allocator);
    }
}

#[test]
fn randomized_block_api_pressure() {
    run_for_page_sizes(|page_size_shift| {
        run_random_test(
            "randomized_block_api_pressure",
            page_size_shift,
            |model, rng| random_block_operation(model, rng),
        );
    });
}

#[test]
fn randomized_at_api_pressure() {
    run_for_page_sizes(|page_size_shift| {
        run_random_test(
            "randomized_at_api_pressure",
            page_size_shift,
            |model, rng| random_at_operation(model, rng),
        );
    });
}

#[test]
fn randomized_mixed_api_pressure() {
    run_for_page_sizes(|page_size_shift| {
        run_random_test(
            "randomized_mixed_api_pressure",
            page_size_shift,
            |model, rng| {
                if rng.bool(1, 2) {
                    random_block_operation(model, rng)
                } else {
                    random_at_operation(model, rng)
                }
            },
        );
    });
}
