//! Direct mappings and early identity mappings with shared physical page records.

use alloc::vec::Vec;

use exarch::mem::{PhysMemoryRegions, PhysMemoryRegionsError, PhysMemoryRegionsStorage};
use expt::opaque::PageTableAction;
use exspin::SpinNoIrq;
use heapless::Vec as HeaplessVec;
use lazyinit::LazyInit;
use log::info;
use memory_addr::{AddrRangeBounds, PhysAddrRange, VirtAddr, va};

use crate::{
    mem::{MemoryRegionFlags, PhysMemoryRegion, pmm, region_flags_to_mapping},
    tlb::TlbInvalidation,
};

use super::{
    MAPPING_TRANSACTION, MappingTransaction, VIRTUAL_ADDRESS_SPACE, direct_mapping_offset,
    finish_local_cleanup, page_size, with_page_table,
};

#[cfg(test)]
mod tests;

/// Inline capacity for page records, including page-rounded no-map barriers during planning.
const MAX_EARLY_MAPPING_REGIONS: usize = pmm::MAX_EARLY_PHYS_MEMORY_REGIONS;

/// Physical page records available before allocation is possible.
type EarlyMappingRegions = PhysMemoryRegions<HeaplessVec<PhysMemoryRegion, MAX_EARLY_MAPPING_REGIONS>>;

/// Physical page records that can grow after the kernel heap is initialized.
type DynamicMappingRegions = PhysMemoryRegions<Vec<PhysMemoryRegion>>;

/// The storage phase of the physical page mapping records.
#[expect(
    clippy::large_enum_variant,
    reason = "inline storage must work before allocation"
)]
enum MappingStorage {
    /// Fixed storage used to construct the initial page tables.
    Early(EarlyMappingRegions),
    /// Dynamic storage used for later device mappings.
    Dynamic(DynamicMappingRegions),
}

/// Storage-independent access to committed mapping records.
impl MappingStorage {
    /// Returns the committed records independently of the storage phase.
    fn as_slice(&self) -> &[PhysMemoryRegion] {
        match self {
            Self::Early(regions) => regions.as_slice(),
            Self::Dynamic(regions) => regions.as_slice(),
        }
    }
}

/// Shared mapping records and the lifecycle of their identity aliases.
struct MappingState {
    /// Nonoverlapping physical page ranges with their actual mapping attributes.
    records: MappingStorage,
    /// Whether every record still has both an identity and a direct-map alias.
    identity_active: bool,
}

/// Mapping metadata serialized with page-table changes by the parent mapping transaction lock.
static PHYSICAL_MAPPINGS: LazyInit<SpinNoIrq<MappingState>> = LazyInit::new();

/// Access and memory-type bits that can affect a page-table mapping.
const MAPPING_ATTRIBUTES: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::EXECUTE)
    .union(MemoryRegionFlags::DEVICE)
    .union(MemoryRegionFlags::UNCACHED);

/// Access bits whose presence requests an actual mapping.
const ACCESS_PERMISSIONS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::EXECUTE);

/// Builds the initial direct and identity aliases from one page-aligned plan.
///
/// Both aliases cover exactly the same physical pages and use identical attributes. No-map
/// bytes exclude their whole base pages. A required boot, kernel, or device mapping sharing such
/// a page is rejected before any page-table entries are created.
pub(in crate::mem) fn init_vmm_mapping_early(regions: &[PhysMemoryRegion]) {
    let records = plan_early_mappings(regions, page_size())
        .expect("incompatible early physical mapping requirements");
    let mut table = VIRTUAL_ADDRESS_SPACE
        .page_table_type_mutex
        .lock()
        .new_pagetable_alloc()
        .expect("failed to allocate early page tables");
    for region in &records {
        let direct = direct_address(region.range);
        let identity = va!(region.range.start.as_usize());
        let flags = region_flags_to_mapping(region.flags);
        table
            .map(identity, region.range.start, region.range.size(), flags)
            .expect("failed to map early identity alias");
        table
            .map(direct, region.range.start, region.range.size(), flags)
            .expect("failed to map early direct alias");
    }
    *VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock() = table;
    PHYSICAL_MAPPINGS.init_once(SpinNoIrq::new(MappingState {
        records: MappingStorage::Early(records),
        identity_active: true,
    }));
}

/// Converts the shared mapping records to dynamic storage after heap initialization.
pub(in crate::mem) fn use_dynamic_mapping_storage() {
    let early = {
        let state = PHYSICAL_MAPPINGS.lock();
        match &state.records {
            MappingStorage::Early(records) => Some(records.clone()),
            MappingStorage::Dynamic(_) => None,
        }
    };
    if let Some(records) = early {
        // The conversion allocates, so it must not run while the mapping-state lock is held.
        let mut dynamic: DynamicMappingRegions = records.into();
        pmm::relocate_region_descriptions(&mut dynamic);
        let mut state = PHYSICAL_MAPPINGS.lock();
        state.records = MappingStorage::Dynamic(dynamic);
    }
}

/// Adds discovered MMIO to physical ownership and maps its previously unmapped pages.
///
/// The BSP invokes this after heap initialization and before AP startup. Existing page attributes
/// must agree, no-map pages are rejected, and regions already given to the physical allocator
/// cannot become MMIO. While identity aliases remain active every new page receives both aliases.
/// All metadata is prepared before the ownership update and page-table writes. Mapping failures
/// during boot are fatal, and mapping records are committed only after both aliases succeed.
pub fn map_mmio_regions(regions: &[PhysMemoryRegion]) {
    let _transaction = MAPPING_TRANSACTION.lock();
    let mut ownership = pmm::clone_final_regions();
    let mut state = PHYSICAL_MAPPINGS.lock();
    let MappingStorage::Dynamic(records) = &state.records else {
        panic!("physical mapping storage is still fixed");
    };
    let mut proposed = records.clone();
    let mut pending = Vec::new();

    for region in regions.iter().copied() {
        assert!(
            region.flags.contains(MemoryRegionFlags::DEVICE)
                && region.flags.contains(MemoryRegionFlags::RESERVED)
                && region.flags.role_sanity_check()
                && region.flags.intersects(ACCESS_PERMISSIONS),
            "invalid platform MMIO region: {region:?}"
        );
        let aligned =
            aligned_mapping_region(region, page_size()).expect("MMIO page alignment failed");
        check_mmio_ownership_pages(ownership.as_slice(), region, page_size())
            .expect("platform MMIO conflicts with physical page ownership");
        ownership
            .insert_overwrite_if(region, |old| {
                old.flags.contains(MemoryRegionFlags::RESERVED)
                    && (!old.flags.intersects(MAPPING_ATTRIBUTES)
                        || old.flags & MAPPING_ATTRIBUTES == region.flags & MAPPING_ATTRIBUTES)
            })
            .expect("platform MMIO conflicts with physical memory ownership");
        check_mapping_attributes(&proposed, aligned)
            .expect("platform MMIO conflicts with an existing mapping page");

        // Keep gap subtraction in PhysMemoryRegions, including partially mapped requests.
        for range in proposed.uncovered(aligned.range) {
            direct_address(range);
            pending.push(PhysMemoryRegion { range, ..aligned });
        }
        proposed
            .insert_uncovered(aligned)
            .expect("failed to prepare new physical mapping records");
    }

    pmm::replace_final_regions(ownership);
    for region in pending {
        info!(
            "Mapping platform MMIO: {} {:#x} flags={}",
            region.desc, region.range, region.flags
        );
        let direct = direct_address(region.range);
        let flags = region_flags_to_mapping(region.flags);
        with_page_table(|table| {
            if state.identity_active {
                table
                    .map(
                        va!(region.range.start.as_usize()),
                        region.range.start,
                        region.range.size(),
                        flags,
                    )
                    .expect("failed to map platform MMIO identity alias");
            }
            table
                .map(direct, region.range.start, region.range.size(), flags)
                .expect("failed to map platform MMIO direct alias");
        });
    }
    state.records = MappingStorage::Dynamic(proposed);
}

/// Removes precisely the identity aliases recorded by the shared physical mapping state.
///
/// The phase changes only after local cleanup and remote TLB acknowledgements complete. All
/// records then continue describing the unchanged direct mappings. Failure after mutation is fatal.
pub(in crate::mem) fn remove_identical_mapping() {
    info!("Removing identical mappings...");
    let transaction = MappingTransaction::begin().expect("identity mapping transaction rejected");
    let records = {
        let state = PHYSICAL_MAPPINGS.lock();
        assert!(state.identity_active, "identity mappings already removed");
        state.records.as_slice().to_vec()
    };
    let cleanup = with_page_table(|table| {
        table
            .apply_actions_with_cleanup(records.iter().map(|region| PageTableAction::Unmap {
                vaddr: va!(region.range.start.as_usize()),
                size: region.range.size(),
            }))
            .expect("identity mapping mutation failed")
    });
    let request = finish_local_cleanup(cleanup, TlbInvalidation::All);
    transaction.complete(request).unwrap_or_else(|error| {
        panic!(
            "identity-map completion failed targets={:?}: {error:?}",
            transaction.targets()
        );
    });
    PHYSICAL_MAPPINGS.lock().identity_active = false;
    info!(
        "identity-map completion targets={:?} operation=All",
        transaction.targets()
    );
}

/// Normalizes byte ranges into disjoint page mappings without allocating.
fn plan_early_mappings(
    regions: &[PhysMemoryRegion],
    page_size: usize,
) -> Result<EarlyMappingRegions, PhysMemoryRegionsError> {
    let mut plan = EarlyMappingRegions::new();
    // Install barriers first so a later rounded neighbor cannot map part of a no-map page.
    for region in regions.iter().copied() {
        if region.flags.contains(MemoryRegionFlags::NO_MAP) {
            plan.insert_uncovered(PhysMemoryRegion {
                range: aligned_range(region.range, page_size)?,
                flags: MemoryRegionFlags::NO_MAP,
                desc: region.desc,
            })?;
        }
    }
    for region in regions.iter().copied() {
        if region.flags.contains(MemoryRegionFlags::NO_MAP)
            || !region.flags.intersects(ACCESS_PERMISSIONS)
        {
            continue;
        }
        let aligned = aligned_mapping_region(region, page_size)?;
        for old in plan.overlaps(aligned.range) {
            if old.flags.contains(MemoryRegionFlags::NO_MAP) {
                let required = MemoryRegionFlags::KERNEL
                    | MemoryRegionFlags::BOOT_SERVICE
                    | MemoryRegionFlags::DEVICE
                    | MemoryRegionFlags::EXECUTE;
                if region.flags.intersects(required) {
                    return Err(PhysMemoryRegionsError::Overlap);
                }
            } else if old.flags & MAPPING_ATTRIBUTES != aligned.flags & MAPPING_ATTRIBUTES {
                return Err(PhysMemoryRegionsError::Overlap);
            }
        }
        plan.insert_uncovered(aligned)?;
    }
    let mut records = EarlyMappingRegions::new();
    for region in &plan {
        if !region.flags.contains(MemoryRegionFlags::NO_MAP) {
            records.insert_disjoint(*region)?;
        }
    }
    Ok(records)
}

/// Rounds a physical byte range outwards and rejects invalid page geometry or overflow.
fn aligned_range(
    range: PhysAddrRange,
    page_size: usize,
) -> Result<PhysAddrRange, PhysMemoryRegionsError> {
    if !page_size.is_power_of_two() || range.start >= range.end {
        return Err(PhysMemoryRegionsError::InvalidRange);
    }
    range
        .align_outwards(page_size)
        .ok_or(PhysMemoryRegionsError::InvalidRange)
}

/// Converts a region into page-aligned mapping attributes with a neutral ownership role.
fn aligned_mapping_region(
    region: PhysMemoryRegion,
    page_size: usize,
) -> Result<PhysMemoryRegion, PhysMemoryRegionsError> {
    Ok(PhysMemoryRegion {
        range: aligned_range(region.range, page_size)?,
        flags: (region.flags & MAPPING_ATTRIBUTES) | MemoryRegionFlags::RESERVED,
        desc: region.desc,
    })
}

/// Rejects any recorded overlap whose actual page attributes disagree with a request.
fn check_mapping_attributes<S: PhysMemoryRegionsStorage>(
    records: &PhysMemoryRegions<S>,
    region: PhysMemoryRegion,
) -> Result<(), PhysMemoryRegionsError> {
    if records
        .overlaps(region.range)
        .any(|old| region_flags_to_mapping(old.flags) != region_flags_to_mapping(region.flags))
    {
        Err(PhysMemoryRegionsError::Overlap)
    } else {
        Ok(())
    }
}

/// Checks every byte owner in the rounded MMIO pages before any mapping is added.
///
/// Even a disjoint subpage MMIO range cannot borrow a page from FREE RAM or required boot data.
/// It also cannot acquire different memory attributes from an already described neighbor.
fn check_mmio_ownership_pages(
    ownership: &[PhysMemoryRegion],
    region: PhysMemoryRegion,
    page_size: usize,
) -> Result<(), PhysMemoryRegionsError> {
    let aligned = aligned_range(region.range, page_size)?;
    for old in ownership {
        if old.range.start >= aligned.end || old.range.end <= aligned.start {
            continue;
        }
        if !old.flags.contains(MemoryRegionFlags::RESERVED)
            || old.flags.intersects(MAPPING_ATTRIBUTES)
                && old.flags & MAPPING_ATTRIBUTES != region.flags & MAPPING_ATTRIBUTES
        {
            return Err(PhysMemoryRegionsError::Overlap);
        }
    }
    Ok(())
}

/// Computes a direct-map address after checking the whole physical range fits its reservation.
fn direct_address(range: PhysAddrRange) -> VirtAddr {
    let window = VIRTUAL_ADDRESS_SPACE.layout.direct_mapping_range;
    assert!(
        range.start < range.end && range.end.as_usize() <= window.size(),
        "physical mapping exceeds direct-map capacity: {range:?}"
    );
    va!(direct_mapping_offset()
        .checked_add(range.start.as_usize())
        .expect("direct-map address overflow"))
}
