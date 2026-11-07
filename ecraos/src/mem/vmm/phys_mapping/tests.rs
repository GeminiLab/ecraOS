//! Page planning tests for direct mappings and early identity mappings.

use memory_addr::{PhysAddrRange, pa};

use crate::mem::{MemoryRegionFlags, PhysMemoryRegion};

use super::{
    EarlyMappingRegions, check_mapping_attributes, check_mmio_ownership_pages, plan_early_mappings,
};

/// Creates one byte-granular region for a page-planning scenario.
fn region(start: usize, end: usize, flags: MemoryRegionFlags) -> PhysMemoryRegion {
    PhysMemoryRegion {
        range: PhysAddrRange::new(pa!(start), pa!(end)),
        flags,
        desc: "test region",
    }
}

/// Compatible byte ranges sharing a page create one physical mapping record.
#[test]
fn compatible_regions_sharing_a_page_are_mapped_once() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::RESERVED;
    let records = plan_early_mappings(
        &[region(0x1100, 0x1800, flags), region(0x1800, 0x1f00, flags)],
        0x1000,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].range,
        PhysAddrRange::new(pa!(0x1000), pa!(0x2000))
    );
}

/// A shared base page cannot have two different access or memory-type attributes.
#[test]
fn incompatible_regions_sharing_a_page_are_rejected() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::RESERVED;
    assert!(
        plan_early_mappings(
            &[
                region(0x1100, 0x1800, flags),
                region(0x1800, 0x1f00, flags | MemoryRegionFlags::DEVICE),
            ],
            0x1000,
        )
        .is_err()
    );
}

/// A subpage no-map reservation excludes the entire base page from ordinary RAM mapping.
#[test]
fn no_map_barriers_exclude_their_whole_page() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::FREE;
    let records = plan_early_mappings(
        &[
            region(0x1000, 0x1800, flags),
            region(0x1800, 0x1900, MemoryRegionFlags::NO_MAP),
            region(0x1900, 0x3000, flags),
        ],
        0x1000,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].range,
        PhysAddrRange::new(pa!(0x2000), pa!(0x3000))
    );
}

/// An essential mapping cannot silently disappear due to a neighboring no-map reservation.
#[test]
fn no_map_page_conflicting_with_boot_or_device_mapping_is_rejected() {
    for role in [MemoryRegionFlags::KERNEL, MemoryRegionFlags::BOOT_SERVICE] {
        assert!(
            plan_early_mappings(
                &[
                    region(0x1000, 0x1800, role | MemoryRegionFlags::READ),
                    region(0x1800, 0x1900, MemoryRegionFlags::NO_MAP),
                ],
                0x1000,
            )
            .is_err()
        );
    }
}

/// Role-only reservations in discovery gaps do not imply a default page mapping.
#[test]
fn reserved_gaps_without_access_permissions_are_not_mapped() {
    let records = plan_early_mappings(
        &[region(0x1000, 0x2000, MemoryRegionFlags::RESERVED)],
        0x1000,
    )
    .unwrap();
    assert!(records.is_empty());
}

/// A supplement uses only holes in a partially mapped interval and rejects attribute changes.
#[test]
fn supplemental_mapping_reuses_existing_pages_and_reports_only_missing_ranges() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::DEVICE | MemoryRegionFlags::RESERVED;
    let mut records = EarlyMappingRegions::new();
    records
        .insert_disjoint(region(0x2000, 0x3000, flags))
        .unwrap();
    let supplement = region(0x1000, 0x4000, flags);
    check_mapping_attributes(&records, supplement).unwrap();
    assert_eq!(
        records
            .uncovered(supplement.range)
            .collect::<alloc::vec::Vec<_>>(),
        [
            PhysAddrRange::new(pa!(0x1000), pa!(0x2000)),
            PhysAddrRange::new(pa!(0x3000), pa!(0x4000)),
        ],
    );
    assert!(
        check_mapping_attributes(
            &records,
            region(0x1000, 0x4000, flags | MemoryRegionFlags::WRITE)
        )
        .is_err()
    );
}

/// MMIO page rounding cannot take bytes from a page that belongs to allocatable RAM.
#[test]
fn mmio_page_sharing_with_free_ram_is_rejected() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::FREE;
    let ownership = [region(0x1000, 0x1800, flags)];
    let mmio = region(
        0x1800,
        0x2000,
        MemoryRegionFlags::READ
            | MemoryRegionFlags::WRITE
            | MemoryRegionFlags::DEVICE
            | MemoryRegionFlags::RESERVED,
    );

    assert!(check_mmio_ownership_pages(&ownership, mmio, 0x1000).is_err());
}
