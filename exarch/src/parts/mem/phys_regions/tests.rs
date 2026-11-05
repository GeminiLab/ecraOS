//! Tests for sorted physical memory region collections.

use alloc::vec::Vec;

use heapless::Vec as HeaplessVec;
use memory_addr::{PhysAddrRange, pa};

use crate::mem::{MemoryRegion, MemoryRegionFlags};

use super::{PhysMemoryRegions, PhysMemoryRegionsError, PhysMemoryRegionsStorage};

/// Creates a byte-precise physical region for a collection scenario.
fn region(start: usize, end: usize, flags: MemoryRegionFlags, desc: &'static str) -> MemoryRegion {
    MemoryRegion {
        range: PhysAddrRange::new(pa!(start), pa!(end)),
        flags,
        desc,
    }
}

/// Treats a fixed-storage reservation as a total count and preserves entries on failure.
#[test]
fn fixed_storage_reserves_total_region_count() {
    let entry = region(0x1000, 0x2000, MemoryRegionFlags::FREE, "ram");
    let mut storage = HeaplessVec::<MemoryRegion, 4>::new();
    storage.push(entry).unwrap();
    storage.push(entry).unwrap();

    storage.reserve_regions(4).unwrap();
    storage.reserve_regions(1).unwrap();
    assert_eq!(storage.as_slice(), &[entry, entry]);
    assert_eq!(
        storage.reserve_regions(5),
        Err(PhysMemoryRegionsError::CapacityExceeded),
    );
    assert_eq!(storage.as_slice(), &[entry, entry]);
    storage.insert_region(2, entry).unwrap();
    storage.insert_region(3, entry).unwrap();
}

/// Reserves a dynamic total without changing existing entries or shrinking capacity.
#[test]
fn dynamic_storage_reserves_total_region_count() {
    let entry = region(0x1000, 0x2000, MemoryRegionFlags::FREE, "ram");
    let mut storage = Vec::from([entry, entry]);

    storage.reserve_regions(4).unwrap();
    assert!(storage.capacity() >= 4);
    let capacity = storage.capacity();
    storage.reserve_regions(1).unwrap();
    assert_eq!(storage.capacity(), capacity);
    assert_eq!(storage.as_slice(), &[entry, entry]);
    storage.insert_region(2, entry).unwrap();
    storage.insert_region(3, entry).unwrap();
    assert_eq!(storage.capacity(), capacity);
}

/// Keeps disjoint regions sorted and rejects overlaps without partial insertion.
#[test]
fn disjoint_insertion_keeps_regions_sorted_and_rejects_overlap_atomically() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 4>>::new();
    let free = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::FREE;
    regions
        .insert_disjoint(region(0x3000, 0x4000, free, "third"))
        .unwrap();
    regions
        .insert_disjoint(region(0x1000, 0x2000, free, "first"))
        .unwrap();
    let before = regions.as_slice().to_vec();

    assert_eq!(
        regions.insert_disjoint(region(0x1800, 0x3800, free, "overlap")),
        Err(PhysMemoryRegionsError::Overlap),
    );
    assert_eq!(regions.as_slice(), before);
    assert_eq!(regions[0].range.start, pa!(0x1000));
    assert_eq!(regions[1].range.start, pa!(0x3000));
}

/// Fills only free gaps and keeps occupied ranges unchanged.
#[test]
fn uncovered_insertion_fills_only_gaps_and_does_not_report_pieces() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 8>>::new();
    let reserved = MemoryRegionFlags::RESERVED;
    let free = MemoryRegionFlags::FREE;
    regions
        .insert_disjoint(region(0x2000, 0x3000, free, "occupied"))
        .unwrap();
    regions
        .insert_disjoint(region(0x5000, 0x6000, free, "occupied"))
        .unwrap();

    regions
        .insert_uncovered(region(0x1000, 0x7000, reserved, "new"))
        .unwrap();

    assert_eq!(
        regions
            .iter()
            .map(|region| region.range)
            .collect::<Vec<_>>(),
        [
            PhysAddrRange::new(pa!(0x1000), pa!(0x2000)),
            PhysAddrRange::new(pa!(0x2000), pa!(0x3000)),
            PhysAddrRange::new(pa!(0x3000), pa!(0x5000)),
            PhysAddrRange::new(pa!(0x5000), pa!(0x6000)),
            PhysAddrRange::new(pa!(0x6000), pa!(0x7000)),
        ]
    );
    assert_eq!(regions[1].flags, free);
    assert_eq!(regions[2].flags, reserved);
}

/// Preserves collection entries before and after an uncovered insertion.
#[test]
fn uncovered_insertion_preserves_regions_before_and_after_its_range() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 8>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x2000, MemoryRegionFlags::FREE, "before"))
        .unwrap();
    regions
        .insert_disjoint(region(0x5000, 0x6000, MemoryRegionFlags::FREE, "after"))
        .unwrap();

    regions
        .insert_uncovered(region(0x3000, 0x4000, MemoryRegionFlags::RESERVED, "new"))
        .unwrap();

    assert_eq!(regions.len(), 3);
    assert_eq!(regions[0].desc, "before");
    assert_eq!(regions[1].desc, "new");
    assert_eq!(regions[2].desc, "after");
}

/// Supplies clipped overlaps to the predicate and fills holes in the new range.
#[test]
fn conditional_overwrite_checks_clipped_overlaps_and_fills_holes() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 8>>::new();
    let free = MemoryRegionFlags::FREE;
    let reserved = MemoryRegionFlags::RESERVED;
    regions
        .insert_disjoint(region(0x1000, 0x3000, free, "left"))
        .unwrap();
    regions
        .insert_disjoint(region(0x5000, 0x7000, free, "right"))
        .unwrap();
    let mut checked = Vec::new();

    regions
        .insert_overwrite_if(region(0x2000, 0x6000, reserved, "overlay"), |overlap| {
            checked.push(overlap.range);
            true
        })
        .unwrap();

    assert_eq!(
        checked,
        [
            PhysAddrRange::new(pa!(0x2000), pa!(0x3000)),
            PhysAddrRange::new(pa!(0x5000), pa!(0x6000)),
        ]
    );
    assert_eq!(regions.len(), 3);
    assert_eq!(
        regions[1].range,
        PhysAddrRange::new(pa!(0x2000), pa!(0x6000))
    );
    assert_eq!(regions[1].flags, reserved);
    assert_eq!(regions[1].desc, "overlay");
    assert_eq!(
        regions[2].range,
        PhysAddrRange::new(pa!(0x6000), pa!(0x7000))
    );
}

/// Leaves the original contents unchanged after rejection or invalid input.
#[test]
fn rejected_overwrite_and_invalid_regions_leave_collection_unchanged() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 2>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x2000, MemoryRegionFlags::FREE, "free"))
        .unwrap();
    let before = regions.as_slice().to_vec();

    assert_eq!(
        regions.insert_overwrite_if(
            region(0x1800, 0x2800, MemoryRegionFlags::RESERVED, "reserved"),
            |_| false,
        ),
        Err(PhysMemoryRegionsError::OverwriteRejected),
    );
    assert_eq!(regions.as_slice(), before);
    assert_eq!(
        regions.insert_disjoint(region(0x2000, 0x2000, MemoryRegionFlags::FREE, "empty")),
        Err(PhysMemoryRegionsError::InvalidRange),
    );
    assert_eq!(
        regions.insert_disjoint(region(0x2000, 0x3000, MemoryRegionFlags::empty(), "bad")),
        Err(PhysMemoryRegionsError::InvalidFlags),
    );
    assert_eq!(regions.as_slice(), before);
}

/// Converts fixed storage to dynamic storage without changing its entries.
#[test]
fn fixed_storage_converts_to_dynamic_storage_without_changing_regions() {
    let mut fixed = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 2>>::new();
    fixed
        .insert_disjoint(region(0x1000, 0x2000, MemoryRegionFlags::FREE, "ram"))
        .unwrap();
    let dynamic: PhysMemoryRegions<Vec<MemoryRegion>> = fixed.into();

    assert_eq!(
        dynamic.as_slice(),
        &[region(0x1000, 0x2000, MemoryRegionFlags::FREE, "ram")]
    );
}

/// Changes only existing intersections and leaves gaps absent.
#[test]
fn transform_changes_only_clipped_intersections_and_keeps_gaps_empty() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 8>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x4000, MemoryRegionFlags::FREE, "ram"))
        .unwrap();

    regions
        .transform_overlaps(PhysAddrRange::new(pa!(0x2000), pa!(0x3000)), |old| {
            MemoryRegion {
                flags: MemoryRegionFlags::RESERVED,
                desc: "reserved",
                ..old
            }
        })
        .unwrap();

    assert_eq!(regions.len(), 3);
    assert_eq!(
        regions[0].range,
        PhysAddrRange::new(pa!(0x1000), pa!(0x2000))
    );
    assert_eq!(
        regions[1].range,
        PhysAddrRange::new(pa!(0x2000), pa!(0x3000))
    );
    assert_eq!(regions[1].flags, MemoryRegionFlags::RESERVED);
    assert_eq!(
        regions[2].range,
        PhysAddrRange::new(pa!(0x3000), pa!(0x4000))
    );
}

/// Rejects a callback result that changes its assigned intersection.
#[test]
fn transform_rejects_a_changed_intersection_range_atomically() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 8>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x4000, MemoryRegionFlags::FREE, "ram"))
        .unwrap();
    let before = regions.as_slice().to_vec();

    assert_eq!(
        regions.transform_overlaps(PhysAddrRange::new(pa!(0x2000), pa!(0x3000)), |old| {
            clipped_for_test(old, 0x1000, 0x3000)
        },),
        Err(PhysMemoryRegionsError::InvalidRange),
    );
    assert_eq!(regions.as_slice(), before);
}

/// Changes a callback result's range to exercise range validation.
fn clipped_for_test(region: MemoryRegion, start: usize, end: usize) -> MemoryRegion {
    MemoryRegion {
        range: PhysAddrRange::new(pa!(start), pa!(end)),
        ..region
    }
}

/// Keeps the source contents unchanged when an insertion exceeds fixed capacity.
#[test]
fn fixed_capacity_failure_keeps_collection_unchanged() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 2>>::new();
    regions
        .insert_disjoint(region(0x2000, 0x3000, MemoryRegionFlags::FREE, "middle"))
        .unwrap();
    regions
        .insert_disjoint(region(0x5000, 0x6000, MemoryRegionFlags::FREE, "far"))
        .unwrap();
    let before = regions.as_slice().to_vec();

    assert_eq!(
        regions.insert_uncovered(region(0x1000, 0x7000, MemoryRegionFlags::RESERVED, "fill")),
        Err(PhysMemoryRegionsError::CapacityExceeded),
    );
    assert_eq!(regions.as_slice(), before);
}

/// Overwriting an interior range keeps both outside fragments in physical-address order.
#[test]
fn overwrite_keeps_both_sides_of_one_old_region() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 3>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x5000, MemoryRegionFlags::FREE, "old"))
        .unwrap();
    let mut calls = 0;
    regions
        .insert_overwrite_if(
            region(0x2000, 0x3000, MemoryRegionFlags::RESERVED, "new"),
            |old| {
                calls += 1;
                assert_eq!(old.range, PhysAddrRange::new(pa!(0x2000), pa!(0x3000)));
                true
            },
        )
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(
        regions[0].range,
        PhysAddrRange::new(pa!(0x1000), pa!(0x2000))
    );
    assert_eq!(
        regions[1].range,
        PhysAddrRange::new(pa!(0x2000), pa!(0x3000))
    );
    assert_eq!(
        regions[2].range,
        PhysAddrRange::new(pa!(0x3000), pa!(0x5000))
    );
}

/// A fixed-capacity transformation or overlay never leaves only part of a split committed.
#[test]
fn transform_and_overlay_capacity_errors_preserve_input_and_do_not_repeat_callbacks() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 2>>::new();
    regions
        .insert_disjoint(region(0x1000, 0x5000, MemoryRegionFlags::FREE, "old"))
        .unwrap();
    let before = regions.as_slice().to_vec();
    for fill_gaps in [false, true] {
        let mut calls = 0;
        let transform = |old: MemoryRegion| {
            calls += 1;
            MemoryRegion {
                flags: MemoryRegionFlags::RESERVED,
                ..old
            }
        };
        let target = region(0x2000, 0x3000, MemoryRegionFlags::RESERVED, "new");
        let result = if fill_gaps {
            regions.overlay_with(target, transform)
        } else {
            regions.transform_overlaps(target.range, transform)
        };
        assert_eq!(result, Err(PhysMemoryRegionsError::CapacityExceeded));
        assert_eq!(calls, 1);
        assert_eq!(regions.as_slice(), before);
    }
}

/// Lookup treats ends as exclusive and adjacent equal-attribute regions remain separate.
#[test]
fn lookup_and_adjacent_regions_preserve_byte_boundaries() {
    let mut regions = PhysMemoryRegions::<HeaplessVec<MemoryRegion, 3>>::new();
    let flags = MemoryRegionFlags::FREE;
    regions
        .insert_disjoint(region(0x1000, 0x1800, flags, "ram"))
        .unwrap();
    regions
        .insert_disjoint(region(0x1800, 0x2000, flags, "ram"))
        .unwrap();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions.find(pa!(0x17ff)).unwrap().range.end, pa!(0x1800));
    assert_eq!(regions.find(pa!(0x1800)).unwrap().range.start, pa!(0x1800));
    assert_eq!(regions.find(pa!(0x2000)), None);
    assert_eq!(
        regions
            .overlaps(PhysAddrRange::new(pa!(0x1800), pa!(0x2000)))
            .count(),
        1
    );
}
