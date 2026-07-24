use core::{alloc::Layout, ptr::NonNull};
use std::{
    alloc::{alloc_zeroed, dealloc},
    thread,
    vec::Vec,
};

use memory_addr::VirtAddr;

use crate::{
    AllocError, SlabAllocResult, SlabAllocator, SlabDeallocResult,
    page::SlabPageHeader,
    size_class::{SLAB_MAX_SIZE, SizeClass},
};

const PAGE_SIZE: usize = 4096;
const OWNER_CPU: u16 = 3;

struct TestPages {
    base: NonNull<u8>,
    layout: Layout,
    pages: usize,
}

impl TestPages {
    fn new(pages: usize) -> Self {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        let base = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("test page allocation");

        Self {
            base,
            layout,
            pages,
        }
    }

    fn base(&self) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(self.base.as_ptr())
    }

    fn bytes(&self) -> usize {
        self.pages * PAGE_SIZE
    }
}

impl Drop for TestPages {
    fn drop(&mut self) {
        unsafe { dealloc(self.base.as_ptr(), self.layout) };
    }
}

fn add_test_slab(allocator: &mut SlabAllocator, size_class: SizeClass, pages: &TestPages) {
    allocator
        .add_slab(size_class, pages.base(), pages.bytes(), OWNER_CPU)
        .expect("valid slab page should be accepted");
}

#[test]
fn size_class_selects_smallest_class_that_satisfies_size_and_alignment() {
    assert_eq!(
        SizeClass::from_layout(Layout::from_size_align(1, 1).unwrap()),
        Some(SizeClass::Bytes8)
    );
    assert_eq!(
        SizeClass::from_layout(Layout::from_size_align(9, 1).unwrap()),
        Some(SizeClass::Bytes16)
    );
    assert_eq!(
        SizeClass::from_layout(Layout::from_size_align(8, 16).unwrap()),
        Some(SizeClass::Bytes16)
    );
    assert_eq!(
        SizeClass::from_layout(Layout::from_size_align(SLAB_MAX_SIZE + 1, 1).unwrap()),
        None
    );
}

#[test]
fn add_slab_rejects_unaligned_base_and_non_page_sized_memory() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let pages = TestPages::new(1);

    assert!(
        allocator
            .add_slab(
                SizeClass::Bytes64,
                pages.base() + 1,
                pages.bytes(),
                OWNER_CPU
            )
            .is_err()
    );
    assert!(
        allocator
            .add_slab(
                SizeClass::Bytes64,
                pages.base(),
                pages.bytes() - 1,
                OWNER_CPU
            )
            .is_err()
    );

    let mut tiny_page_allocator = SlabAllocator::new(64);
    assert!(
        tiny_page_allocator
            .add_slab(SizeClass::Bytes2048, pages.base(), 64, OWNER_CPU)
            .is_err()
    );
}

#[test]
fn empty_allocator_requests_a_slab_for_the_matching_size_class() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let layout = Layout::from_size_align(128, 8).unwrap();

    match allocator.alloc(layout).unwrap() {
        SlabAllocResult::NeedsSlab { size_class, pages } => {
            assert_eq!(size_class, SizeClass::Bytes128);
            assert_eq!(pages, SizeClass::Bytes128.slab_pages(PAGE_SIZE));
        }
        SlabAllocResult::Allocated(_) => panic!("empty allocator should request a slab"),
    }
}

#[test]
fn oversize_layout_is_rejected_by_alloc_and_dealloc() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let layout = Layout::from_size_align(SLAB_MAX_SIZE + 1, 8).unwrap();
    let ptr = NonNull::dangling();

    assert!(matches!(
        allocator.alloc(layout),
        Err(AllocError::InvalidParam)
    ));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        allocator.dealloc(ptr, layout);
    }));
    assert!(result.is_err());
}

#[test]
fn slab_page_header_calculates_object_addresses_and_base() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let pages = TestPages::new(1);
    let size_class = SizeClass::Bytes128;

    add_test_slab(&mut allocator, size_class, &pages);

    let header = unsafe { &*pages.base().as_ptr_of::<SlabPageHeader>() };
    let data_start = header.data_start(pages.base());
    let first_object = header.object_addr(pages.base(), 0);
    let second_object = header.object_addr(pages.base(), 1);

    assert_eq!(first_object, data_start);
    assert_eq!(second_object - first_object, size_class.size());
    assert_eq!(header.object_index(pages.base(), second_object), 1);
    assert_eq!(
        SlabPageHeader::base_from_obj_addr_unknown(second_object, PAGE_SIZE),
        Some(pages.base())
    );
}

#[test]
fn deallocating_second_empty_slab_returns_it_to_caller() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let first_pages = TestPages::new(1);
    let second_pages = TestPages::new(1);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let mut first_slab_objects = Vec::new();
    let mut second_slab_objects = Vec::new();

    add_test_slab(&mut allocator, SizeClass::Bytes64, &first_pages);
    add_test_slab(&mut allocator, SizeClass::Bytes64, &second_pages);

    loop {
        match allocator.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => {
                let base = SlabPageHeader::base_from_obj_addr_unknown(
                    VirtAddr::from_mut_ptr_of(ptr.as_ptr()),
                    PAGE_SIZE,
                )
                .expect("allocated object should belong to a test slab");
                if base == first_pages.base() {
                    first_slab_objects.push(ptr);
                } else if base == second_pages.base() {
                    second_slab_objects.push(ptr);
                } else {
                    panic!("allocated object came from an unexpected slab");
                }
            }
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }

    assert!(!first_slab_objects.is_empty());
    assert!(!second_slab_objects.is_empty());

    for object in first_slab_objects {
        assert!(matches!(
            allocator.dealloc(object, layout),
            SlabDeallocResult::Done
        ));
    }

    let last = second_slab_objects.pop().unwrap();
    for object in second_slab_objects {
        assert!(matches!(
            allocator.dealloc(object, layout),
            SlabDeallocResult::Done
        ));
    }
    assert!(matches!(
        allocator.dealloc(last, layout),
        SlabDeallocResult::FreeSlab { pages: 1, .. }
    ));
}

#[test]
fn remote_free_on_full_slab_is_reclaimed_by_owner_allocation() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let pages = TestPages::new(1);
    let layout = Layout::from_size_align(2048, 8).unwrap();
    let size_class = SizeClass::from_layout(layout).unwrap();
    let mut allocated = Vec::new();

    add_test_slab(&mut allocator, size_class, &pages);

    loop {
        match allocator.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => allocated.push(ptr),
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }

    assert!(!allocated.is_empty());
    let remotely_freed = allocated.pop().unwrap();
    unsafe {
        SlabPageHeader::remote_free_object(remotely_freed, OWNER_CPU, PAGE_SIZE);
    }

    let reclaimed = match allocator.alloc(layout).unwrap() {
        SlabAllocResult::Allocated(ptr) => ptr,
        SlabAllocResult::NeedsSlab { .. } => panic!("remote free should make the slab reusable"),
    };

    assert_eq!(reclaimed, remotely_freed);
}

#[test]
fn concurrent_remote_frees_are_drained_by_owner_allocation() {
    let mut allocator = SlabAllocator::new(PAGE_SIZE);
    let pages = TestPages::new(1);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let size_class = SizeClass::from_layout(layout).unwrap();
    let mut allocated = Vec::new();

    add_test_slab(&mut allocator, size_class, &pages);

    loop {
        match allocator.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => {
                allocated.push(VirtAddr::from_mut_ptr_of(ptr.as_ptr()))
            }
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }

    assert!(allocated.len() >= 4);
    let split_at = allocated.len() / 2;
    let right = allocated.split_off(split_at);
    let left = allocated;

    let left_thread = thread::spawn(move || {
        for addr in left {
            let ptr = NonNull::new(addr.as_mut_ptr()).unwrap();
            unsafe { SlabPageHeader::remote_free_object(ptr, OWNER_CPU, PAGE_SIZE) };
        }
    });
    let right_thread = thread::spawn(move || {
        for addr in right {
            let ptr = NonNull::new(addr.as_mut_ptr()).unwrap();
            unsafe { SlabPageHeader::remote_free_object(ptr, OWNER_CPU, PAGE_SIZE) };
        }
    });

    left_thread.join().unwrap();
    right_thread.join().unwrap();

    let mut reclaimed_count = 0usize;
    loop {
        match allocator.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(_) => reclaimed_count += 1,
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }

    assert!(reclaimed_count >= split_at);
}
