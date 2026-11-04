//! Tests for FDT memory reservation discovery.

use alloc::vec::Vec;

use fdt_rs::error::DevTreeError;
use memory_addr::{PhysAddrRange, pa};

use super::super::reserved::apply_device_tree_reservations;
use super::{TestTree, begin_node, property, word};
use crate::mem::{
    DEFAULT_RAM_FLAGS, MAX_RAW_MEM_REGIONS, MemoryRegion, MemoryRegionFlags, RawMemoryRegions,
};

impl TestTree {
    /// Creates a tree containing static reserved-memory nodes and reservation-map entries.
    fn with_reserved_memory() -> Self {
        let mut structure = Vec::new();
        let mut strings = Vec::new();
        begin_node(&mut structure, b"");
        property(
            &mut structure,
            &mut strings,
            "#address-cells",
            &2u32.to_be_bytes(),
        );
        property(
            &mut structure,
            &mut strings,
            "#size-cells",
            &2u32.to_be_bytes(),
        );
        begin_node(&mut structure, b"reserved-memory");
        property(
            &mut structure,
            &mut strings,
            "#address-cells",
            &2u32.to_be_bytes(),
        );
        property(
            &mut structure,
            &mut strings,
            "#size-cells",
            &2u32.to_be_bytes(),
        );
        property(&mut structure, &mut strings, "ranges", &[]);

        begin_node(&mut structure, b"firmware@80000000");
        property(
            &mut structure,
            &mut strings,
            "reg",
            &[0u32, 0x8000_0000, 0, 0x60000]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        property(&mut structure, &mut strings, "no-map", &[]);
        word(&mut structure, 2);

        begin_node(&mut structure, b"shared-memory@80070000");
        property(
            &mut structure,
            &mut strings,
            "reg",
            &[0u32, 0x8007_0000, 0, 0x10000]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        word(&mut structure, 2);

        begin_node(&mut structure, b"disabled@80080000");
        property(
            &mut structure,
            &mut strings,
            "reg",
            &[0u32, 0x8008_0000, 0, 0x10000]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        property(&mut structure, &mut strings, "status", b"disabled\0");
        word(&mut structure, 2);
        word(&mut structure, 2);
        word(&mut structure, 2);
        word(&mut structure, 9);

        Self::from_parts(structure, strings, &[(0x8005_0000, 0x20000)])
    }
}

/// Applies overlapping FDT reservations, with no-map taking precedence over reserved.
#[test]
fn applies_overlapping_reserved_memory_nodes_and_reservation_map_to_ram() {
    let fixture = TestTree::with_reserved_memory();
    let mut regions = RawMemoryRegions::new();
    regions
        .push(MemoryRegion {
            range: PhysAddrRange::new(pa!(0x8000_0000), pa!(0x8100_0000)),
            flags: DEFAULT_RAM_FLAGS,
            desc: "free memory",
        })
        .unwrap();

    apply_device_tree_reservations(fixture.tree(), &mut regions).unwrap();

    assert_eq!(regions.len(), 5);
    assert_eq!(
        regions[0].range,
        PhysAddrRange::new(pa!(0x8000_0000), pa!(0x8005_0000))
    );
    assert_eq!(
        regions[0].flags,
        MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::NO_MAP
    );
    assert_eq!(
        regions[1].range,
        PhysAddrRange::new(pa!(0x8005_0000), pa!(0x8006_0000))
    );
    assert_eq!(
        regions[1].flags,
        MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::NO_MAP
    );
    assert_eq!(
        regions[2].range,
        PhysAddrRange::new(pa!(0x8006_0000), pa!(0x8007_0000))
    );
    assert_eq!(
        regions[2].flags,
        MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::RESERVED
    );
    assert_eq!(
        regions[3].range,
        PhysAddrRange::new(pa!(0x8007_0000), pa!(0x8008_0000))
    );
    assert_eq!(
        regions[3].flags,
        MemoryRegionFlags::READ | MemoryRegionFlags::WRITE | MemoryRegionFlags::RESERVED
    );
    assert_eq!(
        regions[4].range,
        PhysAddrRange::new(pa!(0x8008_0000), pa!(0x8100_0000))
    );
    assert_eq!(regions[4].flags, DEFAULT_RAM_FLAGS);
}

/// Keeps the input unchanged when reservation splitting exceeds the fixed region capacity.
#[test]
fn reservation_capacity_error_does_not_partially_update_regions() {
    let fixture = TestTree::with_reserved_memory();
    let mut regions = RawMemoryRegions::new();
    regions
        .push(MemoryRegion {
            range: PhysAddrRange::new(pa!(0x8000_0000), pa!(0x8100_0000)),
            flags: DEFAULT_RAM_FLAGS,
            desc: "free memory",
        })
        .unwrap();
    for index in 0..MAX_RAW_MEM_REGIONS - 1 {
        let start = 0x9000_0000 + index * 0x2000;
        regions
            .push(MemoryRegion {
                range: PhysAddrRange::new(pa!(start), pa!(start + 0x1000)),
                flags: DEFAULT_RAM_FLAGS,
                desc: "free memory",
            })
            .unwrap();
    }
    let original_regions = regions.clone();

    assert_eq!(
        apply_device_tree_reservations(fixture.tree(), &mut regions),
        Err(DevTreeError::NotEnoughMemory),
    );
    assert_eq!(regions, original_regions);
}
