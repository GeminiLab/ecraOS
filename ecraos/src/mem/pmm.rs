//! Physical memory ownership and boot-to-dynamic region table handoff.

use alloc::vec::Vec;

use exarch::mem::{MemoryRegion, MemoryRegionFlags, PhysMemoryRegions, RawMemoryRegions};
use exspin::RwLockNoIrq;
use heapless::Vec as HeaplessVec;
use lazyinit::LazyInit;
use memory_addr::{PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};

use crate::mem::{sections, vmm};

#[cfg(test)]
mod tests;

/// The capacity of the kernel region table before the heap allocator is available.
pub const MAX_MEMORY_REGIONS: usize = 64;

/// Fixed-capacity ownership regions used while constructing the early address space.
pub(super) type EarlyMemoryRegions =
    PhysMemoryRegions<HeaplessVec<MemoryRegion, MAX_MEMORY_REGIONS>>;

/// Dynamically extensible ownership regions used after allocator initialization.
pub(super) type DynamicMemoryRegions = PhysMemoryRegions<Vec<MemoryRegion>>;

/// The storage phase of the global ownership table.
///
/// The enum permits a static owner to switch between the two concrete collection types without
/// imposing a storage wrapper on architecture-side discovery.
#[expect(
    clippy::large_enum_variant,
    reason = "inline storage must work before allocation"
)]
enum MemoryRegions {
    /// The allocator is unavailable, so changes use inline storage.
    Early(EarlyMemoryRegions),
    /// The allocator is ready and later device discovery may extend the table.
    Dynamic(DynamicMemoryRegions),
}

/// Storage-independent access to the global ownership table.
impl MemoryRegions {
    /// Borrows the region contents independently of their current storage.
    fn as_slice(&self) -> &[MemoryRegion] {
        match self {
            Self::Early(regions) => regions.as_slice(),
            Self::Dynamic(regions) => regions.as_slice(),
        }
    }
}

/// Ownership-table storage supporting parallel readers and exclusive updates.
type RegionTable = RwLockNoIrq<MemoryRegions>;

/// Ownership regions initialized before creating the kernel page tables.
static PHYS_MEM_REGIONS: LazyInit<RegionTable> = LazyInit::new();

/// Runs a reader without allowing local interrupt or preemption reentry.
///
/// Readers on different CPUs may access the table concurrently. The execution guard is acquired
/// before the read lock and outlives it, so local handlers cannot interrupt a lock holder.
fn read_region_table<R>(table: &RegionTable, reader: impl FnOnce(&MemoryRegions) -> R) -> R {
    let regions = table.read();
    reader(&regions)
}

/// Runs an exclusive update without allowing local interrupt or preemption reentry.
fn write_region_table<R>(table: &RegionTable, writer: impl FnOnce(&mut MemoryRegions) -> R) -> R {
    let mut regions = table.write();
    writer(&mut regions)
}

/// Runs a reader while holding shared access to the physical region table.
///
/// Readers on different CPUs may run concurrently. Local interrupts and preemption are disabled
/// throughout the callback. References cannot escape it. The callback must not recursively access
/// this table, including through console output or logging that can invoke address translation.
///
/// # Panics
///
/// Panics before early memory discovery initializes the table.
pub fn with_phys_mem_regions<R>(reader: impl FnOnce(&[MemoryRegion]) -> R) -> R {
    read_region_table(&PHYS_MEM_REGIONS, |table| reader(table.as_slice()))
}

/// Runs a reader if early discovery has initialized the physical region table.
///
/// The callback has the same locking and borrowing contract as with_phys_mem_regions.
pub fn try_with_phys_mem_regions<R>(reader: impl FnOnce(&[MemoryRegion]) -> R) -> Option<R> {
    let regions = PHYS_MEM_REGIONS.get()?;
    Some(read_region_table(regions, |table| reader(table.as_slice())))
}

/// Copies the early table without allocating or holding its lock during later work.
pub(super) fn clone_early_regions() -> EarlyMemoryRegions {
    let snapshot = read_region_table(&PHYS_MEM_REGIONS, |table| match table {
        MemoryRegions::Early(regions) => Some(regions.clone()),
        MemoryRegions::Dynamic(_) => None,
    });
    snapshot.expect("early region table is no longer available")
}

/// Converts the fixed ownership table to dynamic storage after the heap is initialized.
///
/// The BSP performs this before platform discovery and AP startup.
pub(super) fn use_dynamic_storage() {
    let early = read_region_table(&PHYS_MEM_REGIONS, |table| match table {
        MemoryRegions::Early(regions) => Some(regions.clone()),
        MemoryRegions::Dynamic(_) => None,
    });
    if let Some(regions) = early {
        // The conversion allocates, so do it without holding the ownership-table lock.
        let mut dynamic: DynamicMemoryRegions = regions.into();
        relocate_region_descriptions(&mut dynamic);
        write_region_table(&PHYS_MEM_REGIONS, |table| {
            *table = MemoryRegions::Dynamic(dynamic);
        });
    }
}

/// Refreshes early kernel string references before their low identity aliases are removed.
///
/// Ownership and mapping descriptions are kernel static strings captured before the high-half
/// jump. Copying the region entries does not relocate these runtime pointers automatically.
pub(super) fn relocate_region_descriptions(regions: &mut DynamicMemoryRegions) {
    let (Some(first), Some(last)) = (regions.as_slice().first(), regions.as_slice().last()) else {
        return;
    };
    let range = PhysAddrRange::new(first.range.start, last.range.end);
    regions
        .transform_overlaps(range, |old| {
            let address = VirtAddr::from_ptr_of(old.desc.as_ptr());
            let desc = if vmm::direct_mapping_virt_to_phys(address).is_some() {
                old.desc
            } else {
                let high =
                    vmm::direct_mapping_phys_to_virt(PhysAddr::from_usize(address.as_usize()))
                        .expect("early region description exceeds the direct map");
                // SAFETY: Early descriptions are immutable kernel static strings. Both aliases
                // currently map the same read-only kernel bytes, so length and UTF-8 are unchanged
                // and the high alias remains valid after the identity alias is removed.
                unsafe {
                    core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                        high.as_ptr(),
                        old.desc.len(),
                    ))
                }
            };
            MemoryRegion { desc, ..old }
        })
        .expect("failed to refresh early physical region descriptions");
}

/// Clones the dynamic table so a boot-time device update can be prepared before committing it.
pub(super) fn clone_dynamic_regions() -> DynamicMemoryRegions {
    read_region_table(&PHYS_MEM_REGIONS, |table| {
        let MemoryRegions::Dynamic(regions) = table else {
            panic!("physical region storage is still fixed");
        };
        regions.clone()
    })
}

/// Replaces the dynamic table after an update has passed validation.
pub(super) fn replace_dynamic_regions(regions: DynamicMemoryRegions) {
    let previous = write_region_table(&PHYS_MEM_REGIONS, |table| {
        let MemoryRegions::Dynamic(current) = table else {
            panic!("physical region storage is still fixed");
        };
        core::mem::replace(current, regions)
    });
    drop(previous);
}

/// Converts an identity-mapped kernel section range to its physical range.
fn virt_range_to_id_phys_range(range: VirtAddrRange) -> PhysAddrRange {
    PhysAddrRange::new(
        PhysAddr::from_usize(range.start.as_usize()),
        PhysAddr::from_usize(range.end.as_usize()),
    )
}

/// Builds ownership regions from kernel sections, the loader, and platform discovery.
///
/// Kernel and loader ranges are installed first. Platform reservations and boot data must not
/// overlap them. Free RAM fills only gaps left by those occupied regions.
pub fn build_phys_mem_regions(raw: &RawMemoryRegions, loader_range: PhysAddrRange) {
    let mut regions = EarlyMemoryRegions::new();
    for section in sections::all_sections() {
        if section.aligned_range.start == section.aligned_range.end {
            continue;
        }
        regions
            .insert_disjoint(MemoryRegion {
                range: virt_range_to_id_phys_range(section.aligned_range),
                flags: section.flags | MemoryRegionFlags::KERNEL,
                desc: section.name,
            })
            .expect("invalid or overlapping kernel memory section");
    }
    regions
        .insert_disjoint(MemoryRegion {
            range: loader_range,
            flags: MemoryRegionFlags::BOOT_SERVICE
                | MemoryRegionFlags::READ
                | MemoryRegionFlags::WRITE,
            desc: "loader",
        })
        .expect("invalid or overlapping loader memory region");

    for region in raw {
        if !region.flags.contains(MemoryRegionFlags::FREE) {
            regions
                .insert_disjoint(*region)
                .expect("platform reservation conflicts with kernel or loader memory");
        }
    }
    for region in raw {
        if region.flags.contains(MemoryRegionFlags::FREE) {
            regions
                .insert_uncovered(*region)
                .expect("too many physical memory regions");
        }
    }
    PHYS_MEM_REGIONS.init_once(RegionTable::new(MemoryRegions::Early(regions)));
}
