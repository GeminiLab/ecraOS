//! Correctness checks for the shared runtime page and compile-time layout table.

use core::{alloc::Layout, ptr::NonNull};
use std::{
    alloc::{alloc_zeroed, dealloc},
    collections::BTreeSet,
    sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    vec::Vec,
};

use memory_addr::VirtAddr;

use crate::{
    AllocError, SizeClass, SlabAllocResult, SlabAllocator, SlabDeallocResult, cache::SlabCache,
    page::SlabPageHeader,
};

/// Owned, page-aligned backing memory for a host slab.
struct TestPages {
    /// Start of the backing allocation.
    base: NonNull<u8>,
    /// Allocation layout required when releasing the pages.
    layout: Layout,
}

impl TestPages {
    /// Allocates zeroed backing aligned to the selected page size.
    fn with_page_size(pages: usize, page_size: usize) -> Self {
        let layout = Layout::from_size_align(pages * page_size, page_size).unwrap();
        // SAFETY: The nonzero layout describes the complete allocation retained by this fixture.
        let base = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("test page allocation");

        Self { base, layout }
    }

    /// Returns the virtual base address of the backing pages.
    fn base(&self) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(self.base.as_ptr())
    }
}

impl Drop for TestPages {
    /// Releases the backing allocation after the fixture is no longer used.
    fn drop(&mut self) {
        // SAFETY: All fixture users have finished and the original allocation layout is retained.
        unsafe { dealloc(self.base.as_ptr(), self.layout) };
    }
}

/// Locates immutable ownership from the original allocation geometry without borrowing a cache.
#[test]
fn owner_lookup_uses_original_geometry_without_borrowing_the_allocator() {
    for page_size in [4096, 16384, 65536] {
        for class in SizeClass::ALL {
            let span = (class.size() * 512).min(65536).max(page_size);
            let backing = TestPages::with_page_size(1, span);
            let mut allocator = SlabAllocator::new(page_size).unwrap();
            let owner = 300 + class.index() as u16;
            let request = Layout::from_size_align(1, class.size()).unwrap();
            // SAFETY: The fixture supplies exclusive, complete backing aligned to the slab span.
            unsafe { allocator.add_slab(class, backing.base(), owner) }.unwrap();
            let mut objects = Vec::new();
            while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
                // SAFETY: Every live slot retains its original layout and hardware page size.
                assert_eq!(
                    unsafe { SlabAllocator::owner_cpu(object, request, page_size) },
                    Ok(owner)
                );
                objects.push(object);
            }
            let last = VirtAddr::from_mut_ptr_of(objects.last().unwrap().as_ptr());
            assert!(last >= backing.base() + span - class.size());
            for object in objects {
                // SAFETY: Each live slot is released once by the serialized owner.
                assert!(matches!(
                    unsafe { allocator.dealloc(object, request) },
                    SlabDeallocResult::Done
                ));
            }
        }
    }
    for (request, page_size) in [
        (Layout::from_size_align(8, 8).unwrap(), 8192),
        (Layout::from_size_align(2049, 8).unwrap(), 4096),
        (Layout::from_size_align(1, 4096).unwrap(), 4096),
    ] {
        // SAFETY: Invalid configuration must be rejected before accessing the sentinel address.
        assert_eq!(
            unsafe { SlabAllocator::owner_cpu(NonNull::dangling(), request, page_size) },
            Err(AllocError::InvalidParam)
        );
    }
}

/// Fills every supported geometry and restores summary groups after exhaustion.
#[test]
fn runtime_cache_supports_all_layouts_without_metadata_overlap() {
    for object_log in 3..=11 {
        for slab_log in 12..=18 {
            let object_size = 1 << object_log;
            let span = 1 << slab_log;
            let backing = TestPages::with_page_size(1, span);
            let mut cache = SlabCache::new(object_log, slab_log).unwrap();
            // SAFETY: The fixture owns complete exclusive backing aligned to its entire span.
            unsafe { cache.add_slab(backing.base(), 0) }.unwrap();
            let mut objects = Vec::new();
            let mut addresses = BTreeSet::new();
            while let Some(object) = cache.alloc_object() {
                assert!(object >= backing.base() && object + object_size <= backing.base() + span);
                assert!(object.as_usize().is_multiple_of(object_size));
                assert!(addresses.insert(object));
                // SAFETY: Each returned slot is live, unique, and writable for its complete size.
                unsafe { object.as_mut_ptr_of::<u8>().write_bytes(0xA5, object_size) };
                objects.push(object);
            }
            assert_eq!(objects.len(), cache.layout().slot_count);
            assert_eq!(
                objects.last().copied().unwrap() + object_size,
                backing.base() + span
            );
            let count = objects.len();
            // Independently checks maximal capacity by trying one more aligned bitmap bit/slot.
            let next_offset = (64 + (count + 1).div_ceil(8)).next_multiple_of(object_size);
            assert!(next_offset + (count + 1) * object_size > span);
            for index in [0, count / 2, count - 1] {
                let object = objects[index];
                // SAFETY: The exhausted cache still owns this distinct live slot.
                assert_eq!(unsafe { cache.dealloc_object(object) }, None);
                assert_eq!(cache.alloc_object(), Some(object));
            }
            for object in objects {
                // SAFETY: Each live object is released once to its owning cache.
                assert_eq!(unsafe { cache.dealloc_object(object) }, None);
            }
        }
    }
}

/// Rejects unsupported exponents, hardware pages, and backing alignment before memory access.
#[test]
fn runtime_layout_rejects_invalid_geometry() {
    for (object, slab) in [(2, 12), (12, 12), (3, 11), (3, 19), (usize::MAX, 12)] {
        assert!(matches!(
            SlabCache::new(object, slab),
            Err(AllocError::InvalidParam)
        ));
    }
    for page_size in [0, 2048, 8192, 32768, 131072, usize::MAX] {
        assert!(matches!(
            SlabAllocator::new(page_size),
            Err(AllocError::InvalidParam)
        ));
    }
    let mut cache = SlabCache::new(7, 16).unwrap();
    for base in [VirtAddr::from(0), VirtAddr::from(4096)] {
        // SAFETY: Both invalid bases must be rejected before accessing memory.
        assert_eq!(
            unsafe { cache.add_slab(base, 0) },
            Err(AllocError::InvalidParam)
        );
    }
    let mut allocator = SlabAllocator::new(4096).unwrap();
    let oversize = Layout::from_size_align(2049, 1).unwrap();
    assert!(matches!(
        allocator.alloc(oversize),
        Err(AllocError::InvalidParam)
    ));
    // SAFETY: An invalid layout must be rejected before accessing this sentinel address.
    assert_eq!(
        unsafe { SlabAllocator::remote_free_object(NonNull::dangling(), oversize, 4096) },
        Err(AllocError::InvalidParam)
    );
}

/// Selects the agreed slab policy without confusing hardware page size and backing alignment.
#[test]
fn runtime_allocator_selects_geometry_and_returns_exact_backing() {
    for (page_size, page_counts) in [
        (4096, [1, 2, 4, 8, 16, 16, 16, 16, 16]),
        (16384, [1, 1, 1, 2, 4, 4, 4, 4, 4]),
        (65536, [1, 1, 1, 1, 1, 1, 1, 1, 1]),
    ] {
        for (class, pages) in SizeClass::ALL.into_iter().zip(page_counts) {
            let span = page_size * pages;
            let first = TestPages::with_page_size(1, span);
            let second = TestPages::with_page_size(1, span);
            let mut allocator = SlabAllocator::new(page_size).unwrap();
            let request = Layout::from_size_align(1, class.size()).unwrap();
            let SlabAllocResult::NeedsSlab {
                size_class,
                pages: requested,
            } = allocator.alloc(request).unwrap()
            else {
                panic!("an empty cache must request backing");
            };
            assert_eq!(size_class, class);
            assert_eq!(requested, pages);
            // SAFETY: Both fixtures supply exclusive backing of the requested size and alignment.
            unsafe { allocator.add_slab(class, first.base(), 4) }.unwrap();
            let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() else {
                panic!("registered backing must supply a slot");
            };
            // SAFETY: The live allocation belongs to this owner and retains the first empty slab.
            assert!(matches!(
                unsafe { allocator.dealloc(object, request) },
                SlabDeallocResult::Done
            ));
            // SAFETY: The second fixture is independent and remains live until returned below.
            unsafe { allocator.add_slab(class, second.base(), 4) }.unwrap();
            let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() else {
                panic!("the second backing must supply a slot");
            };
            // SAFETY: This live slot is released exactly once by its serialized owner.
            let SlabDeallocResult::FreeSlab {
                base,
                pages: returned,
            } = (unsafe { allocator.dealloc(object, request) })
            else {
                panic!("the second empty backing must be returned");
            };
            assert_eq!(base, second.base());
            assert_eq!(returned, pages);
        }
    }
}

/// Reuses the same remotely published slot while the owner retains exclusive local state.
#[test]
fn runtime_allocator_reuses_concurrent_remote_publications() {
    let backing = TestPages::with_page_size(1, 65536);
    let mut allocator = SlabAllocator::new(65536).unwrap();
    let request = Layout::from_size_align(8, 8).unwrap();
    // SAFETY: The fixture owns complete writable slab-aligned backing for all scoped operations.
    unsafe { allocator.add_slab(SizeClass::Bytes8, backing.base(), 0) }.unwrap();
    let mut held = Vec::new();
    while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
        held.push(object);
    }
    let object = held.pop().unwrap();
    let (sender, receiver) = mpsc::sync_channel::<VirtAddr>(0);
    thread::scope(|scope| {
        scope.spawn(move || {
            for address in receiver {
                let object = NonNull::new(address.as_mut_ptr()).unwrap();
                // SAFETY: Each message transfers ownership of one live slot with its configuration.
                unsafe { SlabAllocator::remote_free_object(object, request, 65536) }.unwrap();
            }
        });
        for _ in 0..1024 {
            sender
                .send(VirtAddr::from_mut_ptr_of(object.as_ptr()))
                .unwrap();
            let reused = loop {
                if let SlabAllocResult::Allocated(reused) = allocator.alloc(request).unwrap() {
                    break reused;
                }
                thread::yield_now();
            };
            assert_eq!(reused, object);
            // SAFETY: Acquire draining transferred the live slot back to this owner.
            unsafe { reused.as_ptr().write_bytes(0x5A, 8) };
        }
        drop(sender);
    });
    held.push(object);
    for object in held {
        // SAFETY: All producer operations finished and every slot is live exactly once.
        assert!(matches!(
            unsafe { allocator.dealloc(object, request) },
            SlabDeallocResult::Done
        ));
    }
}

/// Returns a remotely emptied slab before waiting for the publishing thread to return.
#[test]
fn runtime_allocator_reclaims_immediately_after_remote_publication() {
    for page_size in [4096, 16384, 65536] {
        let retained = TestPages::with_page_size(1, 65536);
        let reclaimed = TestPages::with_page_size(1, 65536);
        let mut allocator = SlabAllocator::new(page_size).unwrap();
        let request = Layout::from_size_align(2048, 2048).unwrap();
        // SAFETY: Both fixtures supply independent complete backing aligned to 64 KiB.
        unsafe { allocator.add_slab(SizeClass::Bytes2048, retained.base(), 0) }.unwrap();
        let SlabAllocResult::Allocated(first) = allocator.alloc(request).unwrap() else {
            panic!("registered backing must supply a live slot");
        };
        // SAFETY: This live owner allocation is released once to establish the cached empty slab.
        assert!(matches!(
            unsafe { allocator.dealloc(first, request) },
            SlabDeallocResult::Done
        ));
        // SAFETY: The second fixture remains live until explicitly returned below.
        unsafe { allocator.add_slab(SizeClass::Bytes2048, reclaimed.base(), 0) }.unwrap();
        let SlabAllocResult::Allocated(local) = allocator.alloc(request).unwrap() else {
            panic!("the new backing must supply a local slot");
        };
        let SlabAllocResult::Allocated(remote) = allocator.alloc(request).unwrap() else {
            panic!("the new backing must supply a remote slot");
        };
        let address = VirtAddr::from_mut_ptr_of(remote.as_ptr());
        thread::scope(|scope| {
            let producer = scope.spawn(move || {
                let object = NonNull::new(address.as_mut_ptr()).unwrap();
                // SAFETY: This producer publishes one live allocation with its exact geometry.
                unsafe { SlabAllocator::remote_free_object(object, request, page_size) }.unwrap();
            });
            let header = NonNull::new(reclaimed.base().as_mut_ptr_of::<SlabPageHeader>()).unwrap();
            // SAFETY: The local slot keeps the initialized atomic head allocated while waiting.
            while !unsafe { SlabPageHeader::has_remote_frees(header) } {
                thread::yield_now();
            }
            // SAFETY: The final local release drains the published batch and empties backing.
            let SlabDeallocResult::FreeSlab { base, pages } =
                (unsafe { allocator.dealloc(local, request) })
            else {
                panic!("the second empty backing must be returned");
            };
            assert_eq!(base, reclaimed.base());
            assert_eq!(pages * page_size, 65536);
            drop(reclaimed);
            producer.join().unwrap();
        });
    }
}

/// Preserves every remote-list link across complete batches and competing producers.
#[test]
fn runtime_allocator_recovers_batches_from_concurrent_producers() {
    let backing = TestPages::with_page_size(1, 65536);
    let mut allocator = SlabAllocator::new(65536).unwrap();
    let request = Layout::from_size_align(8, 8).unwrap();
    // SAFETY: The fixture remains exclusively registered and live throughout all operations.
    unsafe { allocator.add_slab(SizeClass::Bytes8, backing.base(), 0) }.unwrap();
    let mut addresses = Vec::new();
    while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
        addresses.push(VirtAddr::from_mut_ptr_of(object.as_ptr()));
    }
    let expected: BTreeSet<_> = addresses.iter().copied().collect();
    assert_eq!(expected.len(), 8058);
    for &address in &addresses {
        let object = NonNull::new(address.as_mut_ptr()).unwrap();
        // SAFETY: Each live slot is published once before the owner starts draining the batch.
        unsafe { SlabAllocator::remote_free_object(object, request, 65536) }.unwrap();
    }
    let mut recovered = BTreeSet::new();
    while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
        assert!(recovered.insert(VirtAddr::from_mut_ptr_of(object.as_ptr())));
    }
    assert_eq!(recovered, expected);

    let start = Barrier::new(5);
    let finished = AtomicUsize::new(0);
    let mut recovered = BTreeSet::new();
    thread::scope(|scope| {
        for chunk in addresses.chunks(addresses.len().div_ceil(4)) {
            let start = &start;
            let finished = &finished;
            scope.spawn(move || {
                start.wait();
                for &address in chunk {
                    let object = NonNull::new(address.as_mut_ptr()).unwrap();
                    // SAFETY: Each producer owns a disjoint batch of live slots and publishes once.
                    unsafe { SlabAllocator::remote_free_object(object, request, 65536) }.unwrap();
                }
                finished.fetch_add(1, Ordering::Release);
            });
        }
        start.wait();
        while finished.load(Ordering::Acquire) != 4 {
            if let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
                assert!(recovered.insert(VirtAddr::from_mut_ptr_of(object.as_ptr())));
            } else {
                thread::yield_now();
            }
        }
    });
    while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
        assert!(recovered.insert(VirtAddr::from_mut_ptr_of(object.as_ptr())));
    }
    assert_eq!(recovered, expected);
    for address in recovered {
        let object = NonNull::new(address.as_mut_ptr()).unwrap();
        // SAFETY: The producer phase finished and every recovered slot remains live exactly once.
        assert!(matches!(
            unsafe { allocator.dealloc(object, request) },
            SlabDeallocResult::Done
        ));
    }
}

/// Selects a class satisfying both size and alignment without accepting oversize layouts.
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
        SizeClass::from_layout(Layout::from_size_align(2049, 1).unwrap()),
        None
    );
}

/// Allocates unique slots, reuses remote frees, and reclaims a second empty slab.
#[test]
fn runtime_allocator_preserves_all_class_lifecycles() {
    for page_size in [4096, 16384, 65536] {
        // Fixtures outlive all operations on the allocator, including other classes.
        let mut backing = Vec::new();
        let mut allocator = SlabAllocator::new(page_size).unwrap();
        for size_class in SizeClass::ALL {
            let layout = Layout::from_size_align(size_class.size(), size_class.size()).unwrap();
            let SlabAllocResult::NeedsSlab { pages, .. } = allocator.alloc(layout).unwrap() else {
                panic!("a fresh class must request backing");
            };
            let span = pages * page_size;
            let first = TestPages::with_page_size(1, span);
            // SAFETY: The fixture supplies complete, exclusive, span-aligned backing.
            unsafe { allocator.add_slab(size_class, first.base(), 7) }.unwrap();
            let mut objects = Vec::new();
            let mut addresses = BTreeSet::new();
            while let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() {
                let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
                assert!(addresses.insert(address));
                assert!(
                    address >= first.base() && address + size_class.size() <= first.base() + span
                );
                assert!(address.as_usize().is_multiple_of(size_class.size()));
                // SAFETY: Each slot is live, unique, and writable for its complete class size.
                unsafe { object.as_ptr().write_bytes(0xA5, size_class.size()) };
                objects.push(object);
            }
            assert!(!objects.is_empty());
            let remote = objects.pop().unwrap();
            thread::scope(|scope| {
                let address = VirtAddr::from_mut_ptr_of(remote.as_ptr());
                scope
                    .spawn(move || {
                        let object = NonNull::new(address.as_mut_ptr()).unwrap();
                        // SAFETY: This thread owns one live object with the original layout/config.
                        unsafe { SlabAllocator::remote_free_object(object, layout, page_size) }
                            .unwrap();
                    })
                    .join()
                    .unwrap();
            });
            let SlabAllocResult::Allocated(reused) = allocator.alloc(layout).unwrap() else {
                panic!("remote release must make the full slab usable");
            };
            assert_eq!(reused, remote);
            objects.push(reused);
            for object in objects {
                // SAFETY: Every slot remains live and is released exactly once by its owner.
                assert!(matches!(
                    unsafe { allocator.dealloc(object, layout) },
                    SlabDeallocResult::Done
                ));
            }
            let second = TestPages::with_page_size(1, span);
            // SAFETY: This is a separate complete backing allocation with the required alignment.
            unsafe { allocator.add_slab(size_class, second.base(), 7) }.unwrap();
            let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() else {
                panic!("newly registered slab must supply a slot");
            };
            // SAFETY: The newly allocated slot is live, with the first slab already cached empty.
            let SlabDeallocResult::FreeSlab {
                base,
                pages: returned,
            } = (unsafe { allocator.dealloc(object, layout) })
            else {
                panic!("the second empty slab must be returned");
            };
            assert_eq!(base, second.base());
            assert_eq!(returned, pages);
            drop(second);
            let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() else {
                panic!("the retained empty slab must remain usable");
            };
            let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
            assert!(address >= first.base() && address < first.base() + span);
            // SAFETY: This slot was just allocated from the retained slab.
            assert!(matches!(
                unsafe { allocator.dealloc(object, layout) },
                SlabDeallocResult::Done
            ));
            backing.push(first);
        }
    }
}

/// Reclaims a full-list middle and tail slab through one concrete runtime allocator.
fn check_non_head_unlink(page_size: usize) {
    let class = SizeClass::Bytes2048;
    let span = (class.size() * 512).min(65536).max(page_size);
    let backing: Vec<_> = (0..3).map(|_| TestPages::with_page_size(1, span)).collect();
    let mut allocator = SlabAllocator::new(page_size).unwrap();
    for slab in &backing {
        // SAFETY: Every fixture supplies independent complete backing with full-span alignment.
        unsafe { allocator.add_slab(class, slab.base(), 0) }.unwrap();
    }
    let request = Layout::from_size_align(2048, 2048).unwrap();
    let mut objects = Vec::new();
    while let SlabAllocResult::Allocated(object) = allocator.alloc(request).unwrap() {
        objects.push(object);
    }
    // Registration reverses partial order. Filling reverses full order back to 0, 1, 2.
    for index in [1, 2, 0] {
        let slab = &backing[index];
        let object = *objects
            .iter()
            .find(|object| {
                let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
                address >= slab.base() && address < slab.base() + span
            })
            .expect("every registered slab must provide slots");
        // SAFETY: The selected live slot belongs to this exact allocator and is released once.
        unsafe { SlabAllocator::remote_free_object(object, request, page_size) }.unwrap();
        let SlabAllocResult::Allocated(reused) = allocator.alloc(request).unwrap() else {
            panic!("the owner must find a remote release in any full-list position");
        };
        assert_eq!(reused, object);
        assert!(matches!(
            allocator.alloc(request).unwrap(),
            SlabAllocResult::NeedsSlab { .. }
        ));
    }
    let mut returned = BTreeSet::new();
    for object in objects {
        // SAFETY: Each original allocation, including its reused address, remains live once.
        if let SlabDeallocResult::FreeSlab { base, pages } =
            unsafe { allocator.dealloc(object, request) }
        {
            assert_eq!(pages * page_size, span);
            assert!(returned.insert(base));
        }
    }
    assert_eq!(returned.len(), 2);
    let SlabAllocResult::Allocated(reused) = allocator.alloc(request).unwrap() else {
        panic!("the retained slab must remain linked and reusable");
    };
    let address = VirtAddr::from_mut_ptr_of(reused.as_ptr());
    assert!(backing.iter().any(|slab| !returned.contains(&slab.base())
        && address >= slab.base()
        && address < slab.base() + span));
    // SAFETY: This is the only newly allocated slot in the retained slab.
    assert!(matches!(
        unsafe { allocator.dealloc(reused, request) },
        SlabDeallocResult::Done
    ));
}

/// Preserves middle and tail list membership under every supported hardware page size.
#[test]
fn runtime_allocator_unlinks_non_head_full_slabs() {
    for page_size in [4096, 16384, 65536] {
        check_non_head_unlink(page_size);
    }
}

/// Keeps cache state independent when allocators share the same immutable layout records.
#[test]
fn runtime_allocators_do_not_share_cache_state() {
    let first = TestPages::with_page_size(1, 4096);
    let second = TestPages::with_page_size(1, 4096);
    let mut first_allocator = SlabAllocator::new(4096).unwrap();
    let mut second_allocator = SlabAllocator::new(4096).unwrap();
    let layout = Layout::from_size_align(8, 8).unwrap();
    // SAFETY: Each independent allocator receives distinct complete aligned backing.
    unsafe { first_allocator.add_slab(SizeClass::Bytes8, first.base(), 0) }.unwrap();
    assert!(matches!(
        second_allocator.alloc(layout).unwrap(),
        SlabAllocResult::NeedsSlab { .. }
    ));
    // SAFETY: The second allocation is independent of the first allocator's backing.
    unsafe { second_allocator.add_slab(SizeClass::Bytes8, second.base(), 1) }.unwrap();
    for (allocator, base) in [
        (&mut first_allocator, first.base()),
        (&mut second_allocator, second.base()),
    ] {
        let mut count = 0;
        while let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() {
            let address = VirtAddr::from_mut_ptr_of(object.as_ptr());
            assert!(address >= base && address < base + 4096);
            count += 1;
        }
        assert_eq!(count, 496);
    }
}
