//! Physical memory management region table builder.

use exarch::mem::{MemoryRegion, MemoryRegionFlags, RawMemoryRegions};
use heapless::Vec as HeaplessVec;
use lazyinit::LazyInit;
use memory_addr::{AddrRangeBounds, PhysAddr, PhysAddrRange, VirtAddrRange};

use crate::mem::sections;

/// The maximum number of memory regions finally built.
pub const MAX_MEMORY_REGIONS: usize = 64;
/// The final physical memory regions.
pub type MemoryRegions = HeaplessVec<MemoryRegion, MAX_MEMORY_REGIONS>;

/// Static storage for the physical memory region table.
///
/// Written once by [`set_phys_mem_regions`] and read back via
/// [`saved_phys_mem_regions`].
static PHYS_MEM_REGIONS: LazyInit<MemoryRegions> = LazyInit::new();

/// Retrieves the physical memory regions.
///
/// # Panics
///
/// Panics if the physical memory regions have not been initialized yet.
pub fn phys_mem_regions() -> &'static MemoryRegions {
    PHYS_MEM_REGIONS.as_ref()
}

/// Retrieves the physical memory regions if they have been initialized.
pub fn try_phys_mem_regions() -> Option<&'static MemoryRegions> {
    PHYS_MEM_REGIONS.get()
}

/// Pushes a physical memory region into a [`MemoryRegions`], panicking if the
/// region overlaps with any existing region.
///
/// # Panics
///
/// Panics if the region overlaps with any existing region or if the number of
/// regions exceeds the maximum.
fn push_phys_mem_region_no_overlap(target: &mut MemoryRegions, region: MemoryRegion) {
    for other in target.iter() {
        if region.range.overlaps(other.range) {
            panic!("physical memory region {region:#x?} overlaps with {other:#x?}");
        }
    }

    target.push(region).expect("too many memory regions");
}

const MAX_SPLIT_SUB_REGIONS: usize = 8;

fn subtract_overlaps<I>(
    range: PhysAddrRange,
    occupied: I,
) -> HeaplessVec<PhysAddrRange, MAX_SPLIT_SUB_REGIONS>
where
    I: Iterator<Item = PhysAddrRange>,
{
    let mut remaining = HeaplessVec::new();
    if !range.is_empty() {
        remaining
            .push(range)
            .expect("failed to initialize physical range subtraction");
    }

    for other in occupied {
        let current = core::mem::replace(&mut remaining, HeaplessVec::new());
        for candidate in current {
            let (before, after) = candidate.subtract(other);
            if let Some(before) = before.filter(|range| !range.is_empty()) {
                remaining
                    .push(before)
                    .expect("splitting into too many memory regions");
            }
            if let Some(after) = after.filter(|range| !range.is_empty()) {
                remaining
                    .push(after)
                    .expect("splitting into too many memory regions");
            }
        }
    }

    remaining
}

/// Pushes a physical memory region into a [`MemoryRegions`], splitting the
/// region into multiple parts to skip overlapping parts if necessary.
///
/// # Panics
///
/// Panics if the region would be split into too many parts or the number of
/// regions exceeds the maximum.
fn push_phys_mem_region_skip_overlap(target: &mut MemoryRegions, region: MemoryRegion) {
    sort_phys_mem_regions(target);
    let occupied = target.iter().map(|region| region.range);
    let remaining = subtract_overlaps(region.range, occupied);
    target.extend(remaining.into_iter().map(|range| MemoryRegion {
        range,
        flags: region.flags,
        desc: region.desc,
    }));
}

/// Sorts a [`MemoryRegions`] by address range start.
fn sort_phys_mem_regions(regions: &mut MemoryRegions) {
    regions.sort_unstable_by_key(|region| region.range.start);
}

/// Checks if a [`MemoryRegions`] is valid.
fn check_phys_mem_regions(regions: &MemoryRegions) {
    for region in regions.iter() {
        if !region.flags.role_sanity_check() {
            panic!("physical memory region {region:#x?} has invalid role flags");
        }
    }

    for [a, b] in regions.array_windows() {
        if a.range.end > b.range.start {
            panic!("physical memory regions {a:#x?} and {b:#x?} overlap");
        }
    }
}

/// Converts a virtual address range to an identically-mapped physical address
/// range.
fn virt_range_to_id_phys_range(virt_range: VirtAddrRange) -> PhysAddrRange {
    PhysAddrRange::new(
        PhysAddr::from_usize(virt_range.start.as_usize()),
        PhysAddr::from_usize(virt_range.end.as_usize()),
    )
}

/// Builds the final physical memory region table.
///
/// The final memory regions contains:
/// - All kernel sections with the [`MemoryRegionFlags::KERNEL`] flag set.
/// - The loader range with the [`MemoryRegionFlags::BOOT_SERVICE`] flag set.
/// - All boot-service regions reported by [`exarch::mem::raw_mem_regions`].
/// - All reserved regions reported by [`exarch::mem::raw_mem_regions`].
/// - All free regions reported by [`exarch::mem::raw_mem_regions`], with
///   portions overlapping with other regions dropped.
pub fn build_phys_mem_regions(raw_mem_regions: &RawMemoryRegions, loader_range: PhysAddrRange) {
    let mut final_mem_regions = MemoryRegions::new();

    for section in sections::all_sections() {
        push_phys_mem_region_no_overlap(
            &mut final_mem_regions,
            MemoryRegion {
                range: virt_range_to_id_phys_range(section.aligned_range),
                flags: section.flags.union(MemoryRegionFlags::KERNEL),
                desc: section.name,
            },
        );
    }

    push_phys_mem_region_no_overlap(
        &mut final_mem_regions,
        MemoryRegion {
            range: loader_range,
            // No EXECUTE flag because the loader should no longer be called.
            flags: MemoryRegionFlags::BOOT_SERVICE
                | MemoryRegionFlags::READ
                | MemoryRegionFlags::WRITE,
            desc: "loader",
        },
    );

    for raw_region in raw_mem_regions {
        if raw_region.flags.contains(MemoryRegionFlags::RESERVED)
            || raw_region.flags.contains(MemoryRegionFlags::BOOT_SERVICE)
        {
            push_phys_mem_region_no_overlap(&mut final_mem_regions, *raw_region)
        }
    }

    for raw_region in raw_mem_regions {
        if raw_region.flags.contains(MemoryRegionFlags::FREE) {
            push_phys_mem_region_skip_overlap(&mut final_mem_regions, *raw_region);
        }
    }

    sort_phys_mem_regions(&mut final_mem_regions);
    check_phys_mem_regions(&final_mem_regions);

    PHYS_MEM_REGIONS.init_once(final_mem_regions);
}

#[cfg(test)]
mod tests {
    use memory_addr::{AddrRangeBounds, PhysAddrRange, pa};

    use super::subtract_overlaps;

    #[test]
    fn subtract_overlaps_removes_multiple_occupied_segments() {
        let target = PhysAddrRange::new(pa!(0x1000), pa!(0x9000));
        let occupied = [
            PhysAddrRange::new(pa!(0x2000), pa!(0x3000)),
            PhysAddrRange::new(pa!(0x5000), pa!(0x6000)),
        ];

        let remaining = subtract_overlaps(target, occupied.iter().copied());

        assert_eq!(
            remaining.as_slice(),
            &[
                PhysAddrRange::new(pa!(0x1000), pa!(0x2000)),
                PhysAddrRange::new(pa!(0x3000), pa!(0x5000)),
                PhysAddrRange::new(pa!(0x6000), pa!(0x9000)),
            ]
        );
    }

    #[test]
    fn subtract_overlaps_drops_fully_covered_and_keeps_adjacent_ranges() {
        let target = PhysAddrRange::new(pa!(0x2000), pa!(0x6000));
        let occupied = [
            PhysAddrRange::new(pa!(0x1000), pa!(0x7000)),
            PhysAddrRange::new(pa!(0x7000), pa!(0x8000)),
        ];

        let remaining = subtract_overlaps(target, occupied.iter().copied());

        assert!(remaining.is_empty());

        let target = PhysAddrRange::new(pa!(0x2000), pa!(0x6000));
        let occupied = [
            PhysAddrRange::new(pa!(0x1000), pa!(0x2000)),
            PhysAddrRange::new(pa!(0x6000), pa!(0x7000)),
        ];

        let remaining = subtract_overlaps(target, occupied.iter().copied());

        assert_eq!(remaining.as_slice(), &[target]);
        assert!(remaining[0].is_valid());
    }
}
