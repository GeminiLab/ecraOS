//! Physical memory management region table builder.

use core::mem::MaybeUninit;

use explat::mem::{MemoryRegion, MemoryRegionFlags, MemoryRegions};
use heapless::Vec as HeaplessVec;
use memory_addr::{PhysAddr, PhysAddrRange, VirtAddrRange};

use crate::mem::sections;

/// The maximum number of physical memory regions after splitting.
pub const MAX_PHYS_MEM_REGIONS: usize = 128;

/// A physical memory region with a debug label.
///
/// Each entry pairs a [`MemoryRegion`] with a static string label identifying
/// its origin (e.g., "free", "loader", "kernel .text").
pub type PhysMemRegions = HeaplessVec<(MemoryRegion, &'static str), MAX_PHYS_MEM_REGIONS>;

/// The default flags for a free RAM gap after splitting.
///
/// Free gaps retain readability, writability, and the FREE flag so the buddy
/// allocator can reclaim them later.
const FREE_GAP_FLAGS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::FREE);

/// Static storage for the physical memory region table.
///
/// Written once by [`set_phys_mem_regions`] and read back via
/// [`saved_phys_mem_regions`].
static mut PHYS_MEM_REGIONS: MaybeUninit<PhysMemRegions> = MaybeUninit::uninit();

/// Saves the physical memory regions for later retrieval.
///
/// # Safety
///
/// Must be called exactly once, before any call to [`saved_phys_mem_regions`].
pub fn set_phys_mem_regions(regions: PhysMemRegions) {
    unsafe {
        (&raw mut PHYS_MEM_REGIONS).as_mut().unwrap().write(regions);
    }
}

/// Retrieves the saved physical memory regions.
///
/// # Panics
///
/// Panics if [`set_phys_mem_regions`] has not been called yet.
pub fn saved_phys_mem_regions() -> &'static PhysMemRegions {
    unsafe {
        (&raw const PHYS_MEM_REGIONS)
            .as_ref()
            .unwrap()
            .assume_init_ref()
    }
}

/// Converts a kernel-section virtual address range to a physical address range.
///
/// Subtracts the virtual-to-physical offset so that the returned range
/// corresponds to the actual physical addresses occupied by the section.
fn virt_range_to_phys(virt_range: VirtAddrRange, vpo: usize) -> PhysAddrRange {
    PhysAddrRange::new(
        PhysAddr::from_usize(virt_range.start.as_usize() - vpo),
        PhysAddr::from_usize(virt_range.end.as_usize() - vpo),
    )
}

/// A single exclusion applied during region splitting.
///
/// Each exclusion carries a physical address range, the flags to assign to the
/// overlapped portion, and a human-readable label.
struct Exclusion {
    /// The physical address range to exclude.
    range: PhysAddrRange,
    /// Flags for the excluded sub-region.
    flags: MemoryRegionFlags,
    /// Debug label for the excluded sub-region.
    label: &'static str,
}

/// Builds the final physical memory region table.
///
/// Splits raw platform regions around the loader range and kernel sections,
/// producing a unified table with flag-annotated regions. Each output entry
/// carries a label that identifies its purpose (free gap, loader, kernel
/// section, or reserved).
///
/// # Algorithm
///
/// 1. Non-FREE boot regions are pushed unchanged with the label "reserved".
/// 2. FREE boot regions are iteratively split around each exclusion (loader,
///    then each kernel section in order). Gaps between exclusions retain
///    [`FREE_GAP_FLAGS`] with the label "free".
pub fn build_phys_mem_regions(
    boot_regions: &MemoryRegions,
    loader_range: PhysAddrRange,
    virt_phys_offset: usize,
) -> PhysMemRegions {
    let exclusions = build_exclusions(loader_range, virt_phys_offset);
    let mut result = PhysMemRegions::new();

    for boot_region in boot_regions {
        if boot_region.flags.contains(MemoryRegionFlags::FREE) {
            split_free_region(boot_region, &exclusions, &mut result);
        } else {
            push_region(
                &mut result,
                boot_region.range,
                boot_region.flags,
                "reserved",
            );
        }
    }

    result
}

/// Collects all exclusion ranges (loader + kernel sections).
///
/// The loader range is listed first, followed by each kernel section in the
/// canonical order produced by [`sections::all_sections`]. Only the
/// page-aligned variants are used so that splits land on page boundaries.
fn build_exclusions(
    loader_range: PhysAddrRange,
    virt_phys_offset: usize,
) -> HeaplessVec<Exclusion, { sections::SECTION_COUNT + 1 }> {
    let mut exclusions: HeaplessVec<Exclusion, { sections::SECTION_COUNT + 1 }> =
        HeaplessVec::new();

    exclusions
        .push(Exclusion {
            range: loader_range,
            flags: MemoryRegionFlags::BOOT_SERVICE
                | MemoryRegionFlags::READ
                | MemoryRegionFlags::WRITE,
            label: "loader",
        })
        .ok();

    for (name, _exact, aligned) in sections::all_sections() {
        exclusions
            .push(Exclusion {
                range: virt_range_to_phys(aligned, virt_phys_offset),
                flags: section_flags(name),
                label: section_label(name),
            })
            .ok();
    }

    exclusions
}

/// Returns the permission flags for a kernel section by name.
fn section_flags(name: &str) -> MemoryRegionFlags {
    match name {
        "text" => MemoryRegionFlags::READ | MemoryRegionFlags::EXECUTE,
        "rodata" => MemoryRegionFlags::READ,
        "data" | "rela_dyn" | "got" | "bss" => MemoryRegionFlags::READ | MemoryRegionFlags::WRITE,
        _ => MemoryRegionFlags::READ | MemoryRegionFlags::WRITE,
    }
}

/// Returns the display label for a kernel section by name.
///
/// # Panics
///
/// Panics if the section name is not one of the six known kernel sections.
fn section_label(name: &str) -> &'static str {
    match name {
        "text" => "kernel .text",
        "rodata" => "kernel .rodata",
        "data" => "kernel .data",
        "rela_dyn" => "kernel .rela_dyn",
        "got" => "kernel .got",
        "bss" => "kernel .bss",
        _ => unreachable!("unknown kernel section: {name}"),
    }
}

/// Splits a single FREE boot region around all exclusion ranges.
///
/// Starts with one sub-region (the full FREE region) and iteratively applies
/// each exclusion. Overlapping sub-regions are replaced by up to three pieces:
/// a left gap, the excluded part, and a right gap.
fn split_free_region(
    boot_region: &MemoryRegion,
    exclusions: &[Exclusion],
    result: &mut PhysMemRegions,
) {
    // Working list of (range, flags, label) tuples.
    let mut sub_regions: HeaplessVec<
        (PhysAddrRange, MemoryRegionFlags, &'static str),
        MAX_PHYS_MEM_REGIONS,
    > = HeaplessVec::new();

    sub_regions
        .push((boot_region.range, FREE_GAP_FLAGS, "free"))
        .ok();

    for exclusion in exclusions {
        let exc_start = exclusion.range.start;
        let exc_end = exclusion.range.end;

        let mut next: HeaplessVec<
            (PhysAddrRange, MemoryRegionFlags, &'static str),
            MAX_PHYS_MEM_REGIONS,
        > = HeaplessVec::new();

        for (range, flags, label) in &sub_regions {
            // No overlap — keep the sub-region unchanged.
            if !range.overlaps(exclusion.range) {
                next.push((*range, *flags, *label)).ok();
                continue;
            }

            // Left gap: [range.start .. exc_start)
            if range.start < exc_start
                && let Some(gap) = PhysAddrRange::try_new(range.start, exc_start)
            {
                next.push((gap, *flags, *label)).ok();
            }

            // Excluded part: [max(start, exc_start) .. min(end, exc_end))
            let overlap_start = range.start.max(exc_start);
            let overlap_end = range.end.min(exc_end);
            if overlap_start < overlap_end
                && let Some(overlap) = PhysAddrRange::try_new(overlap_start, overlap_end)
            {
                next.push((overlap, exclusion.flags, exclusion.label)).ok();
            }

            // Right gap: [exc_end .. range.end)
            if exc_end < range.end
                && let Some(gap) = PhysAddrRange::try_new(exc_end, range.end)
            {
                next.push((gap, *flags, *label)).ok();
            }
        }

        sub_regions = next;
    }

    // Push all surviving sub-regions into the output.
    for (range, flags, label) in sub_regions {
        push_region(result, range, flags, label);
    }
}

/// Pushes a region into the result, skipping empty ranges.
fn push_region(
    result: &mut PhysMemRegions,
    range: PhysAddrRange,
    flags: MemoryRegionFlags,
    label: &'static str,
) {
    if range.is_empty() {
        return;
    }
    result.push((MemoryRegion { range, flags }, label)).ok();
}
