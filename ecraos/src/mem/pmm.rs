//! Physical memory management region table builder.

use explat::mem::{MemoryRegion, MemoryRegionFlags, RawMemoryRegions};
use heapless::Vec as HeaplessVec;
use lazyinit::LazyInit;
use memory_addr::{PhysAddr, PhysAddrRange, VirtAddrRange};

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

/// Pushes a physical memory region into a [`MemoryRegions`], splitting the
/// region into multiple parts to skip overlapping parts if necessary.
///
/// # Panics
///
/// Panics if the region would be split into too many parts or the number of
/// regions exceeds the maximum.
fn push_phys_mem_region_skip_overlap(target: &mut MemoryRegions, region: MemoryRegion) {
    const MAX_SPLIT_SUB_REGIONS: usize = 8;

    let MemoryRegion {
        range: mut region_range,
        flags,
        desc,
    } = region;
    let mut to_be_pushed: HeaplessVec<MemoryRegion, MAX_SPLIT_SUB_REGIONS> = HeaplessVec::new();
    sort_phys_mem_regions(target);

    for other in &target[0..target.len()] {
        if other.range.start <= region_range.start {
            // Case 1, the start of the region may be included in the other
            // region.
            if other.range.end <= region_range.start {
                // Case 1a, it's actually not! Go to the next region.
                continue;
            } else if other.range.end >= region_range.end {
                // Case 1b, the region is completely included in the other
                // region. Break the loop because the region is completely
                // consumed.
                break;
            } else {
                // Case 1c, the region is partially included in the other
                // region. Remove the overlapping part.
                region_range.start = other.range.end;
                continue;
            }
        } else {
            // Case 2, the start of the region is before the start of the other
            // region.
            if other.range.start >= region_range.end {
                // Case 2a, the end of the region is before the start of the
                // other region, i.e. the region is completely before the other
                // region. Push the region and return.
                to_be_pushed
                    .push(MemoryRegion {
                        range: region_range,
                        flags,
                        desc,
                    })
                    .expect("splitting into too many memory regions");
                region_range.start = region_range.end;
                break;
            } else {
                // Case 2b, the end of the region is after the start of the
                // other region, i.e. the region partially overlaps with the
                // other region. Pushes the non-overlapping part.
                to_be_pushed
                    .push(MemoryRegion {
                        range: PhysAddrRange::new(region_range.start, other.range.start),
                        flags,
                        desc,
                    })
                    .expect("splitting into too many memory regions");

                if other.range.end >= region_range.end {
                    // Case 2b1, the end of the other region is after the end
                    // of the region, i.e. the region is completely consumed.
                    break;
                } else {
                    // Case 2b2, the end of the other region is before the end
                    // of the region, i.e. the region is partially consumed.
                    region_range.start = other.range.end;
                }
            }
        }
    }

    if !region_range.is_empty() {
        to_be_pushed
            .push(MemoryRegion {
                range: region_range,
                flags,
                desc,
            })
            .expect("splitting into too many memory regions");
    }

    target.extend(to_be_pushed);
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
/// - All reserved regions reported by [`explat::mem::raw_mem_regions`].
/// - All free regions reported by [`explat::mem::raw_mem_regions`], with
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
            | raw_region.flags.contains(MemoryRegionFlags::BOOT_SERVICE)
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

// /// Collects all exclusion ranges (loader + kernel sections).
// ///
// /// The loader range is listed first, followed by each kernel section in the
// /// canonical order produced by [`sections::all_sections`]. Only the
// /// page-aligned variants are used so that splits land on page boundaries.
// fn build_exclusions(
//     loader_range: PhysAddrRange,
// ) -> HeaplessVec<Exclusion, { sections::SECTION_COUNT + 1 }> {
//     let mut exclusions: HeaplessVec<Exclusion, { sections::SECTION_COUNT + 1 }> =
//         HeaplessVec::new();

//     exclusions
//         .push(Exclusion {
//             range: loader_range,
//             flags: MemoryRegionFlags::BOOT_SERVICE
//                 | MemoryRegionFlags::READ
//                 | MemoryRegionFlags::WRITE,
//             label: "loader",
//         })
//         .ok();

//     for (name, _exact, aligned) in sections::all_sections() {
//         exclusions
//             .push(Exclusion {
//                 range: virt_range_to_id_phys_range(aligned),
//                 flags: section_flags(name),
//                 label: section_label(name),
//             })
//             .ok();
//     }

//     exclusions
// }

// /// Returns the permission flags for a kernel section by name.
// fn section_flags(name: &str) -> MemoryRegionFlags {
//     match name {
//         "text" => MemoryRegionFlags::READ | MemoryRegionFlags::EXECUTE,
//         "rodata" => MemoryRegionFlags::READ,
//         "data" | "rela_dyn" | "got" | "bss" => MemoryRegionFlags::READ | MemoryRegionFlags::WRITE,
//         _ => MemoryRegionFlags::READ | MemoryRegionFlags::WRITE,
//     }
// }

// /// Returns the display label for a kernel section by name.
// ///
// /// # Panics
// ///
// /// Panics if the section name is not one of the six known kernel sections.
// fn section_label(name: &str) -> &'static str {
//     match name {
//         "text" => "kernel .text",
//         "rodata" => "kernel .rodata",
//         "data" => "kernel .data",
//         "rela_dyn" => "kernel .rela_dyn",
//         "got" => "kernel .got",
//         "bss" => "kernel .bss",
//         _ => unreachable!("unknown kernel section: {name}"),
//     }
// }

// /// Maximum working sub-regions during splitting.
// ///
// /// Each exclusion can at most double the count. With 7 exclusions and 1
// /// initial region, the theoretical maximum is 15. 32 provides ample headroom
// /// while keeping stack usage low (~1 KiB vs ~4 KiB for 128).
// const MAX_SPLIT_SUB_REGIONS: usize = 32;

// /// Splits a single FREE boot region around all exclusion ranges.
// ///
// /// Starts with one sub-region (the full FREE region) and iteratively applies
// /// each exclusion. Overlapping sub-regions are replaced by up to three pieces:
// /// a left gap, the excluded part, and a right gap.
// fn split_free_region(
//     boot_region: &MemoryRegion,
//     exclusions: &[Exclusion],
//     result: &mut PhysMemRegions,
// ) {
//     // Working list of (range, flags, label) tuples.
//     let mut sub_regions: HeaplessVec<
//         (PhysAddrRange, MemoryRegionFlags, &'static str),
//         MAX_SPLIT_SUB_REGIONS,
//     > = HeaplessVec::new();

//     sub_regions
//         .push((boot_region.range, FREE_GAP_FLAGS, "free"))
//         .ok();

//     for exclusion in exclusions {
//         let exc_start = exclusion.range.start;
//         let exc_end = exclusion.range.end;

//         let mut next: HeaplessVec<
//             (PhysAddrRange, MemoryRegionFlags, &'static str),
//             MAX_SPLIT_SUB_REGIONS,
//         > = HeaplessVec::new();

//         for (range, flags, label) in &sub_regions {
//             // No overlap — keep the sub-region unchanged.
//             if !range.overlaps(exclusion.range) {
//                 next.push((*range, *flags, *label)).ok();
//                 continue;
//             }

//             // Left gap: [range.start .. exc_start)
//             if range.start < exc_start
//                 && let Some(gap) = PhysAddrRange::try_new(range.start, exc_start)
//             {
//                 next.push((gap, *flags, *label)).ok();
//             }

//             // Excluded part: [max(start, exc_start) .. min(end, exc_end))
//             let overlap_start = range.start.max(exc_start);
//             let overlap_end = range.end.min(exc_end);
//             if overlap_start < overlap_end
//                 && let Some(overlap) = PhysAddrRange::try_new(overlap_start, overlap_end)
//             {
//                 next.push((overlap, exclusion.flags, exclusion.label)).ok();
//             }

//             // Right gap: [exc_end .. range.end)
//             if exc_end < range.end
//                 && let Some(gap) = PhysAddrRange::try_new(exc_end, range.end)
//             {
//                 next.push((gap, *flags, *label)).ok();
//             }
//         }

//         sub_regions = next;
//     }

//     // Push all surviving sub-regions into the output.
//     for (range, flags, label) in sub_regions {
//         push_region(result, range, flags, label);
//     }
// }

// /// Pushes a region into the result, skipping empty ranges.
// fn push_region(
//     result: &mut PhysMemRegions,
//     range: PhysAddrRange,
//     flags: MemoryRegionFlags,
//     label: &'static str,
// ) {
//     if range.is_empty() {
//         return;
//     }
//     result.push((MemoryRegion { range, flags }, label)).ok();
// }
