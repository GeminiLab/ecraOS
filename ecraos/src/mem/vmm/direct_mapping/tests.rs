//! Page planning tests for direct mappings and early identity mappings.

use exarch::mem::PhysMemoryRegionsError;
use memory_addr::{PhysAddrRange, pa};

use crate::mem::{MemoryRegionFlags, PhysMemoryRegion, pmm};

use super::{EarlyRegions, plan_formal_mappings, plan_mappings};

/// Creates one byte-granular region for a page-planning scenario.
fn region(start: usize, end: usize, flags: MemoryRegionFlags) -> PhysMemoryRegion {
    PhysMemoryRegion {
        range: PhysAddrRange::new(pa!(start), pa!(end)),
        flags,
        desc: "test region",
    }
}

/// Verifies that compatible subpage regions produce one record for their shared page.
#[test]
fn compatible_regions_sharing_a_page_are_mapped_once() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::RESERVED;
    let records: EarlyRegions = plan_mappings(
        &[region(0x1100, 0x1800, flags), region(0x1800, 0x1f00, flags)],
        0x1000,
        true,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].range,
        PhysAddrRange::new(pa!(0x1000), pa!(0x2000))
    );
}

/// Verifies that incompatible attributes on a shared base page are rejected.
#[test]
fn incompatible_regions_sharing_a_page_are_rejected() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::RESERVED;
    assert_eq!(
        plan_mappings::<pmm::EarlyRegionsStorage>(
            &[
                region(0x1100, 0x1800, flags),
                region(0x1800, 0x1f00, flags | MemoryRegionFlags::DEVICE),
            ],
            0x1000,
            true,
        )
        .unwrap_err(),
        PhysMemoryRegionsError::Overlap,
    );
}

/// Verifies that a `NO_MAP` region excludes its whole page from the mapping plan.
#[test]
fn no_map_barriers_exclude_their_whole_page() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::FREE;
    let records: EarlyRegions = plan_mappings(
        &[
            region(0x1000, 0x1800, flags),
            region(0x1800, 0x1900, MemoryRegionFlags::NO_MAP),
            region(0x1900, 0x3000, flags),
        ],
        0x1000,
        true,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].range,
        PhysAddrRange::new(pa!(0x2000), pa!(0x3000))
    );
}

/// Verifies that `NO_MAP` page overlaps with required mappings are rejected.
///
/// Covers kernel, boot-service, device, and executable mappings.
#[test]
fn no_map_page_conflicting_with_required_mapping_is_rejected() {
    for flags in [
        MemoryRegionFlags::KERNEL | MemoryRegionFlags::READ,
        MemoryRegionFlags::BOOT_SERVICE | MemoryRegionFlags::READ,
        MemoryRegionFlags::RESERVED | MemoryRegionFlags::READ | MemoryRegionFlags::DEVICE,
        MemoryRegionFlags::RESERVED | MemoryRegionFlags::READ | MemoryRegionFlags::EXECUTE,
    ] {
        assert_eq!(
            plan_mappings::<pmm::EarlyRegionsStorage>(
                &[
                    region(0x1000, 0x1800, flags),
                    region(0x1800, 0x1900, MemoryRegionFlags::NO_MAP),
                ],
                0x1000,
                true,
            )
            .unwrap_err(),
            PhysMemoryRegionsError::Overlap,
        );
    }
}

/// Verifies that reserved regions without access permissions produce no mapping records.
#[test]
fn reserved_gaps_without_access_permissions_are_not_mapped() {
    let records: EarlyRegions = plan_mappings(
        &[region(0x1000, 0x2000, MemoryRegionFlags::RESERVED)],
        0x1000,
        true,
    )
    .unwrap();
    assert!(records.is_empty());
}

/// Verifies that compatible regions cover their combined rounded range with unchanged flags.
#[test]
fn compatible_regions_cover_their_combined_page_ranges() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::DEVICE | MemoryRegionFlags::RESERVED;
    let records = plan_formal_mappings(
        &[region(0x1100, 0x2800, flags), region(0x2800, 0x3f00, flags)],
        0x1000,
        true,
    )
    .unwrap();
    assert_eq!(
        records
            .uncovered(PhysAddrRange::new(pa!(0x1000), pa!(0x4000)))
            .next(),
        None,
    );
    assert!(records.iter().all(|record| record.flags == flags));
}

/// Verifies that an MMIO region cannot share a page with free RAM.
#[test]
fn mmio_page_sharing_with_free_ram_is_rejected() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::FREE;
    let mmio = region(
        0x1800,
        0x2000,
        MemoryRegionFlags::READ
            | MemoryRegionFlags::WRITE
            | MemoryRegionFlags::DEVICE
            | MemoryRegionFlags::RESERVED,
    );

    assert_eq!(
        plan_formal_mappings(&[region(0x1000, 0x1800, flags), mmio], 0x1000, true).unwrap_err(),
        PhysMemoryRegionsError::Overlap,
    );
}

/// Verifies that formal mapping records preserve page rounding and exclude unmapped gaps.
#[test]
fn formal_records_preserve_the_initial_page_plan() {
    let ram = MemoryRegionFlags::FREE | MemoryRegionFlags::READ | MemoryRegionFlags::WRITE;
    let boot = MemoryRegionFlags::BOOT_SERVICE | MemoryRegionFlags::READ;
    let records = plan_formal_mappings(
        &[
            region(0x1000, 0x1800, ram),
            region(0x1800, 0x1900, MemoryRegionFlags::NO_MAP),
            region(0x1900, 0x3000, ram),
            region(0x3000, 0x4000, MemoryRegionFlags::RESERVED),
            region(0x4100, 0x4800, boot),
        ],
        0x1000,
        true,
    )
    .unwrap();

    assert_eq!(
        records.as_slice(),
        &[region(0x2000, 0x3000, ram), region(0x4000, 0x5000, boot),]
    );
}

/// Verifies that formal mapping planning rejects conflicting page attributes.
#[test]
fn formal_records_reject_incompatible_page_attributes() {
    let flags = MemoryRegionFlags::RESERVED | MemoryRegionFlags::READ;
    assert_eq!(
        plan_formal_mappings(
            &[
                region(0x1100, 0x1800, flags),
                region(0x1800, 0x1f00, flags | MemoryRegionFlags::DEVICE),
            ],
            0x1000,
            true,
        )
        .unwrap_err(),
        PhysMemoryRegionsError::Overlap,
    );
}

/// Verifies that update plans retain whole `NO_MAP` pages without access permissions.
#[test]
fn update_plan_retains_no_map_pages_without_access_permissions() {
    let ram = MemoryRegionFlags::FREE | MemoryRegionFlags::READ | MemoryRegionFlags::WRITE;
    let no_map = MemoryRegionFlags::NO_MAP;
    let records = plan_formal_mappings(
        &[
            region(0x1000, 0x1800, ram),
            region(0x1800, 0x1900, no_map),
            region(0x1900, 0x3000, ram),
        ],
        0x1000,
        false,
    )
    .unwrap();

    assert_eq!(
        records.as_slice(),
        &[region(0x1000, 0x2000, no_map), region(0x2000, 0x3000, ram),],
    );
}

/// Verifies that a `NO_MAP` replacement remains in the update plan after page rounding.
#[test]
fn no_map_replacement_remains_in_the_update_plan() {
    let flags = MemoryRegionFlags::READ | MemoryRegionFlags::NO_MAP;
    let records = plan_formal_mappings(&[region(0x1100, 0x1f00, flags)], 0x1000, false).unwrap();

    assert_eq!(records.as_slice(), &[region(0x1000, 0x2000, flags)]);
}
