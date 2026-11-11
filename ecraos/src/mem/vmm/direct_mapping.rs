//! Physical memory region direct mappings and temporary identity mappings.

use exarch::mem::{PhysMemoryRegions, PhysMemoryRegionsError, PhysMemoryRegionsStorage};
use expanic::bug;
use expt::{opaque::PageTableAction, pt::cleanup::TlbFlushes};
use log::info;
use memory_addr::{AddrRangeBounds, PhysAddrRange, VirtAddr, va};

use crate::mem::{MemoryRegionFlags, PhysMemoryRegion, pmm, region_flags_to_mapping};

use super::{
    EarlyPhase, MappingTransaction, VIRTUAL_ADDRESS_SPACE, VmmState, direct_mapping_offset,
    finish_local_cleanup, page_size, state::StableFormalPhase,
};

#[cfg(test)]
mod tests;

/// Page-aligned direct mapping records stored before heap initialization.
///
/// Uses the same fixed-capacity storage as [`pmm::EarlyRegions`].
///
/// May include `NO_MAP` ranges to record pages excluded during mapping updates.
pub(super) type EarlyRegions = pmm::EarlyRegions;

/// Page-aligned direct mapping records stored after heap initialization.
///
/// Uses the same dynamically extensible storage as [`pmm::FinalRegions`].
///
/// May include `NO_MAP` ranges to record pages excluded during mapping updates.
pub(super) type FinalRegions = pmm::FinalRegions;

/// Access and memory-type bits in memory region flags that can affect the flag bits of page table
/// entries.
const PAGE_TABLE_BITS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::EXECUTE)
    .union(MemoryRegionFlags::DEVICE)
    .union(MemoryRegionFlags::UNCACHED);

/// Access bits in memory region flags whose presence requests an actual mapping.
const ACCESS_BITS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::EXECUTE);

/// Bits in memory region flags that prevent the range from being shrunk during planning.
const NO_SHRINK_BITS: MemoryRegionFlags = MemoryRegionFlags::KERNEL
    .union(MemoryRegionFlags::BOOT_SERVICE)
    .union(MemoryRegionFlags::DEVICE)
    .union(MemoryRegionFlags::EXECUTE);

/// Creates the initial page table and maps the discovered physical memory regions.
///
/// Each physical memory region is processed according to the
/// [physical-region mapping rules](crate::mem::vmm#physical-region-mappings) and mapped twice (
/// unless it's ignored as per the rules): mapped to the direct mapping area and identity-mapped to
/// the low-address space.
///
/// # Panics
///
/// Panics unless the layout is initialized and no page table has been created. Also panics if
/// provided physical memory regions fail to meet the requirements, allocation fails, or a page
/// table cannot be created.
///
/// # Safety
///
/// The caller must ensure that the regions are valid: regions must remain sorted and
/// non-overlapping, every range must be nonempty, and all flags must remain valid.
/// Valid flags contain only defined bits and exactly one role.
pub(in crate::mem) unsafe fn init_vmm_mapping_early(regions: &[PhysMemoryRegion]) {
    // Plan the mappings.
    let plan: EarlyRegions = plan_mappings(regions, page_size(), true)
        .expect("early physical regions cannot be mapped properly");

    // Write-lock the virtual address space state and check it.
    let mut state = VIRTUAL_ADDRESS_SPACE.state.write();
    let VmmState::LayoutReady { page_table_type } = &*state else {
        bug!(
            r#"`init_vmm_mapping_early` called with invalid state, "layout ready" expected but got {}"#,
            state.desc()
        );
    };

    // Allocate the early page table.
    let mut page_table = page_table_type
        .new_pagetable_alloc()
        .expect("failed to allocate early page tables");

    // Map the regions to the early page table.
    for region in &plan {
        let direct = direct_mapping_address(region.range);
        let identity = va!(region.range.start.as_usize());
        let flags = region_flags_to_mapping(region.flags);
        page_table
            .map(identity, region.range.start, region.range.size(), flags)
            .expect("failed to map early identity mapping");
        page_table
            .map(direct, region.range.start, region.range.size(), flags)
            .expect("failed to map early direct mapping");
    }

    // Update the state to the early page table ready phase.
    *state = VmmState::Early {
        page_table,
        phase: EarlyPhase::TableReady,
    };
}

/// Removes low-address identity mappings while preserving direct mappings.
///
/// This function waits for local cleanup and remote CPUs to invalidate the removed translations.
///
/// # Panics
///
/// Panics if identity mappings are not active or if they cannot be fully removed.
pub(in crate::mem) fn remove_identity_mapping() {
    info!("Removing identity mappings...");

    // Begin a mapping transaction to remove the identity mappings.
    let mut transaction =
        MappingTransaction::begin().expect("identity mapping transaction rejected");
    assert_eq!(
        transaction.previous_phase,
        StableFormalPhase::IdentityActive
    );

    // Remove the identity mappings from the page table.
    transaction.with_page_table_mut(|table, records, remote_flushes| {
        let cleanup = table
            .apply_actions_with_cleanup(records.iter().map(|region| PageTableAction::Unmap {
                vaddr: va!(region.range.start.as_usize()),
                size: region.range.size(),
            }))
            .expect("identity mapping removal failed");
        remote_flushes.append(TlbFlushes::Full);
        finish_local_cleanup(cleanup, remote_flushes);
    });

    // Complete the transaction (waiting for remote CPUs to invalidate TLB).
    info!("Waiting for remote TLB invalidation...");
    transaction
        .perform_remote_flushes()
        .unwrap_or_else(|error| {
            panic!(
                "identity mapping removal completion failed, targets: {:?}, error: {error:?}",
                transaction.targets()
            );
        });

    // Finish the transaction and update the state to the formal mappings active phase.
    transaction.finish_to(StableFormalPhase::Active);

    info!("Identity mappings removed");
}

/// Plans direct mappings using heap-backed region storage.
///
/// Uses [`plan_mappings`] with dynamically extensible storage. Heap allocation must be available.
pub(super) fn plan_formal_mappings(
    regions: &[PhysMemoryRegion],
    page_size: usize,
    remove_no_map: bool,
) -> Result<FinalRegions, PhysMemoryRegionsError> {
    plan_mappings::<pmm::FinalRegionsStorage>(regions, page_size, remove_no_map)
}

/// Adds or overwrites physical regions and updates their direct mappings.
///
/// Requires heap allocation, dynamically extensible PMM storage, and a page table managed by the
/// buddy allocator. Each region must be nonempty. Its flags must contain only defined bits,
/// exactly one role, and at least one of the `READ`, `WRITE`, and `EXECUTE` bits.
///
/// Also updates low-address identity mappings if identity mappings have not been removed. New
/// `NO_MAP` regions remove existing mappings over their entire rounded pages.
///
/// Returns after completing local and required remote TLB invalidations.
///
/// # Overlap rules
///
/// 1. Before alignment, byte ranges must not overlap, as required by [`PhysMemoryRegions`], unless
///    `overwrite_existing_if(new, old)` returns true for each existing intersection. Regions are
///    inserted in slice order, so existing regions include earlier entries from `regions`. Only
///    the overlapping part of an old region is replaced. The callback receives the complete new
///    region and the old region clipped to that intersection, with its original flags and
///    description.
/// 2. After outward alignment to page boundaries, regions must satisfy the
///    [physical-region mapping rules](crate::mem::vmm#physical-region-mappings).
///    Compatible overlaps are allowed, but incompatible overlaps are rejected even if
///    `overwrite_existing_if` permitted replacement of the original byte ranges.
///
/// # Calling this function
///
/// Do not call while holding a PMM table lock. The callback must not start another mapping
/// operation, including through [`crate::mem::allocs::vmalloc`].
///
/// This function itself disables preemption and local interrupts when necessary. The caller must
/// **NOT** disable local interrupts when calling this function if the current CPU has been marked
/// as IPI-ready, as it may cause deadlocks if another CPU is updating the page table.
///
/// Calling this function is expensive. Prefer passing several regions in one call.
///
/// # Panics
///
/// Panics if initialization or interrupt requirements are not met, a region is invalid or
/// conflicts with existing physical regions or mappings, or mapping or TLB invalidation fails.
///
/// A panic does not undo physical region additions and page table changes that have been applied.
pub fn add_and_map_phys_regions<F>(regions: &[PhysMemoryRegion], mut overwrite_existing_if: F)
where
    F: FnMut(PhysMemoryRegion, PhysMemoryRegion) -> bool,
{
    let mut transaction = MappingTransaction::begin().expect("MMIO mapping transaction rejected");
    let page_size = page_size();
    let mut appended_phys_regions = pmm::clone_final_regions();

    for region in regions.iter().copied() {
        assert!(
            region.flags.role_sanity_check() && region.flags.intersects(ACCESS_BITS),
            "invalid new physical region: {region:?}"
        );

        appended_phys_regions
            .insert_overwrite_if(region, |old| overwrite_existing_if(region, old))
            .unwrap_or_else(|err| panic!("new physical region conflicts with existing physical regions, region: {region:?}, error: {err:?}"));
    }

    // Rebuild and reapply the whole plan: checking every interaction between aligned old and new
    // regions incrementally is considerably more complex. Region additions are infrequent, so
    // repeated page-table updates and TLB invalidations are an accepted design tradeoff.
    let new_plan = plan_formal_mappings(appended_phys_regions.as_slice(), page_size, false)
        .unwrap_or_else(|err| panic!("failed to plan new physical regions, error: {err:?}"));

    pmm::replace_final_regions(appended_phys_regions);

    /// Converts page-aligned region records into mapping or unmapping actions.
    fn regions_to_actions<'a>(
        regions: &'a [PhysMemoryRegion],
        va_addr_fn: impl Fn(PhysAddrRange) -> VirtAddr + 'a,
    ) -> impl Iterator<Item = PageTableAction<VirtAddr>> + 'a {
        regions.iter().map(move |region| {
            if region.flags.contains(MemoryRegionFlags::NO_MAP) {
                PageTableAction::Unmap {
                    vaddr: va_addr_fn(region.range),
                    size: region.range.size(),
                }
            } else {
                PageTableAction::Map {
                    vaddr: va_addr_fn(region.range),
                    paddr: region.range.start,
                    size: region.range.size(),
                    flags: region_flags_to_mapping(region.flags),
                }
            }
        })
    }

    transaction.append_remote_flushes(TlbFlushes::Full);
    transaction
        .apply_actions(regions_to_actions(
            new_plan.as_slice(),
            direct_mapping_address,
        ))
        .expect("failed to update the page table with new physical regions");

    transaction
        .perform_remote_flushes()
        .expect("new physical regions mapping completion failed");

    if transaction.previous_phase == StableFormalPhase::IdentityActive {
        transaction.append_remote_flushes(TlbFlushes::Full);
        transaction
            .apply_actions(regions_to_actions(
                new_plan.as_slice(),
                identity_mapping_address,
            ))
            .expect("failed to update the page table with new physical regions");

        transaction
            .perform_remote_flushes()
            .expect("new physical regions mapping completion failed");
    }

    transaction.with_page_table_mut(|_, records, _| core::mem::replace(records, new_plan));
}

/// Plans how to map a list of physical regions.
///
/// `regions` must satisfy the range and flag invariants of [`PhysMemoryRegions`]. Uses the
/// [physical-region mapping rules](crate::mem::vmm#physical-region-mappings), preserving flags and
/// descriptions. When `remove_no_map` is false, the result also contains rounded `NO_MAP` ranges
/// so callers can remove existing mappings. Otherwise, `NO_MAP` ranges only exclude pages during
/// planning and will be removed from the result before returning.
///
/// # Errors
///
/// Returns an error if alignment fails, mapping requirements conflict, or the plan cannot be
/// stored.
fn plan_mappings<S: PhysMemoryRegionsStorage>(
    regions: &[PhysMemoryRegion],
    page_size: usize,
    remove_no_map: bool,
) -> Result<PhysMemoryRegions<S>, PhysMemoryRegionsError> {
    let mut plan = PhysMemoryRegions::<S>::new();

    // Add `NO_MAP` regions first.
    for region in regions.iter() {
        if region.flags.contains(MemoryRegionFlags::NO_MAP) {
            plan.insert_uncovered(PhysMemoryRegion {
                range: region
                    .range
                    .align_outwards(page_size)
                    .ok_or(PhysMemoryRegionsError::InvalidRange)?,
                flags: region.flags,
                desc: region.desc,
            })?;
        }
    }

    // Add other regions.
    for region in regions.iter() {
        // Skip `NO_MAP` regions and regions without access permissions. They will not be mapped.
        if region.flags.contains(MemoryRegionFlags::NO_MAP) || !region.flags.intersects(ACCESS_BITS)
        {
            continue;
        }

        let aligned = align_region(region, page_size)?;

        for old in plan.iter() {
            if !are_aligned_regions_compatible(old, &aligned) {
                return Err(PhysMemoryRegionsError::Overlap);
            }
        }

        plan.insert_uncovered(aligned)?;
    }

    if remove_no_map {
        plan.retain(|region| !region.flags.contains(MemoryRegionFlags::NO_MAP));
    }
    Ok(plan)
}

/// Checks if two outwardly aligned regions are compatible (can be mapped together).
fn are_aligned_regions_compatible(first: &PhysMemoryRegion, second: &PhysMemoryRegion) -> bool {
    if !first.range.overlaps(second.range) {
        return true;
    }

    let first_no_map = first.flags.contains(MemoryRegionFlags::NO_MAP);
    let second_no_map = second.flags.contains(MemoryRegionFlags::NO_MAP);

    if first_no_map && second_no_map {
        true
    } else if first_no_map || second_no_map {
        let non_no_map = if first_no_map { second } else { first };

        !non_no_map.flags.intersects(NO_SHRINK_BITS)
    } else {
        first.flags & PAGE_TABLE_BITS == second.flags & PAGE_TABLE_BITS
    }
}

/// Rounds a physical byte range outwards to page boundaries.
fn align_region(
    region: &PhysMemoryRegion,
    page_size: usize,
) -> Result<PhysMemoryRegion, PhysMemoryRegionsError> {
    Ok(PhysMemoryRegion {
        range: region
            .range
            .align_outwards(page_size)
            .ok_or(PhysMemoryRegionsError::InvalidRange)?,
        flags: region.flags,
        desc: region.desc,
    })
}

/// Computes a direct-map address after checking the whole physical range fits its reservation.
///
/// # Panics
///
/// Panics if the layout is unavailable, the range is empty or reversed, or it exceeds the
/// direct-map capacity.
fn direct_mapping_address(range: PhysAddrRange) -> VirtAddr {
    let window = VIRTUAL_ADDRESS_SPACE.layout.direct_mapping_area;
    assert!(
        range.start < range.end && range.end.as_usize() <= window.size(),
        "physical mapping exceeds direct-map capacity: {range:?}"
    );
    va!(direct_mapping_offset()
        .checked_add(range.start.as_usize())
        .expect("direct-map address overflow"))
}

/// Computes a low-address identity mapping address.
fn identity_mapping_address(range: PhysAddrRange) -> VirtAddr {
    va!(range.start.as_usize())
}
