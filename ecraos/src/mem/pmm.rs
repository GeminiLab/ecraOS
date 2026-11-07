//! Physical memory region management.
//!
//! Maintains the kernel's global table of physical memory regions. Each
//! [`PhysMemoryRegion`] describes a physical address range, its flags, and a
//! description. Regions are nonempty, sorted by physical address, and nonoverlapping.
//! Their boundaries retain byte precision.
//!
//! The table combines platform memory information with kernel and loader reservations
//! and can be extended with discovered device regions. Page allocators use it to
//! identify usable RAM, and [`super::vmm`] uses it to prepare physical-memory mappings.
//! A `FREE` region identifies RAM eligible for page allocation. Individual page
//! allocations are tracked by [`super::allocs::palloc`].
//!
//! Use [`lock_read_phys_mem_regions`] to inspect the table under a shared lock, or
//! [`try_lock_read_phys_mem_regions`] to avoid waiting for conflicting lock holders.
//! Write access requires an exclusive lock and preserving the region invariants.
//! Callbacks run with local interrupts and preemption disabled.
//!
//! The table is initialized during early boot with a fixed capacity. After heap
//! initialization, its storage can be switched to support additional regions without
//! the early capacity limit.

use alloc::vec::Vec;

use exarch::mem::{MemoryRegionFlags, PhysMemoryRegion, PhysMemoryRegions, RawMemoryRegions};
use exspin::RwLockNoIrq;
use heapless::Vec as HeaplessVec;
use lazyinit::LazyInit;
use memory_addr::{AddrRangeBounds, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};

use crate::mem::{sections, vmm};

#[cfg(test)]
mod tests;

/// The capacity of the kernel physical memory region table before the heap allocator is available.
pub(crate) const MAX_EARLY_PHYS_MEMORY_REGIONS: usize = 64;

/// Fixed-capacity physical memory regions used before the heap allocator is available.
pub(crate) type EarlyRegions = PhysMemoryRegions<HeaplessVec<PhysMemoryRegion, MAX_EARLY_PHYS_MEMORY_REGIONS>>;

/// Dynamically extensible physical memory regions used after the heap allocator is available.
pub(crate) type FinalRegions = PhysMemoryRegions<Vec<PhysMemoryRegion>>;

/// A [`PhysMemoryRegions`] that is backed by either a `HeaplessVec` or a `Vec`.
///
/// The enum allows the unification of both storage types and the in-place replacement of the
/// storage type, and is used to store the physical memory regions in the kernel.
#[expect(
    clippy::large_enum_variant,
    reason = "inline storage must work before allocation"
)]
enum RuntimeRegions {
    /// The heap allocator is unavailable, so `HeaplessVec` is being used.
    Early(EarlyRegions),
    /// The heap allocator is ready and `Vec` is being used.
    Final(FinalRegions),
}

/// Storage-independent access to the underlying storage.
impl RuntimeRegions {
    /// Borrows the region contents independently of their current storage.
    fn as_slice(&self) -> &[PhysMemoryRegion] {
        match self {
            Self::Early(regions) => regions.as_slice(),
            Self::Final(regions) => regions.as_slice(),
        }
    }

    /// Borrows the region contents mutably independently of their current storage.
    ///
    /// # Safety
    ///
    /// The caller must preserve the region invariants: regions must remain sorted and
    /// non-overlapping, every range must be nonempty, and all flags must remain valid.
    /// Valid flags contain only defined bits and exactly one role.
    unsafe fn as_mut_slice(&mut self) -> &mut [PhysMemoryRegion] {
        // SAFETY: The caller promises to maintain the invariant.
        unsafe {
            match self {
                Self::Early(regions) => regions.as_mut_slice(),
                Self::Final(regions) => regions.as_mut_slice(),
            }
        }
    }
}

/// [`RuntimeRegions`] wrapped in a [`RwLockNoIrq`] to allow concurrent reads and exclusive writes.
type RegionTable = RwLockNoIrq<RuntimeRegions>;

/// The global physical memory regions table.
static PHYS_MEM_REGIONS: LazyInit<RegionTable> = LazyInit::new();

/// Acquires a shared read lock on the physical memory regions table and runs a reader while holding
/// it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table.
fn read_region_table<R>(table: &RegionTable, reader: impl FnOnce(&RuntimeRegions) -> R) -> R {
    let regions = table.read();
    reader(&regions)
}

/// Tries to acquire a shared read lock on the physical memory regions table and run a reader while
/// holding it.
///
/// Returns `None` if the lock could not be acquired.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table.
fn try_read_region_table<R>(
    table: &RegionTable,
    reader: impl FnOnce(&RuntimeRegions) -> R,
) -> Option<R> {
    let regions = table.try_read()?;
    Some(reader(&regions))
}

/// Acquires an exclusive write lock on the physical memory regions table and runs a writer while
/// holding it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table.
fn write_region_table<R>(table: &RegionTable, writer: impl FnOnce(&mut RuntimeRegions) -> R) -> R {
    let mut regions = table.write();
    writer(&mut regions)
}

/// Tries to acquire an exclusive write lock and runs the writer if successful.
///
/// Returns `None` if the lock could not be acquired.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table.
fn try_write_region_table<R>(
    table: &RegionTable,
    writer: impl FnOnce(&mut RuntimeRegions) -> R,
) -> Option<R> {
    let mut regions = table.try_write()?;
    Some(writer(&mut regions))
}

/// Errors that can occur when trying to lock the global physical memory regions table.
#[derive(Debug, thiserror::Error)]
pub enum PhysMemoryRegionsLockError {
    /// The global physical memory regions table has not been initialized yet.
    #[error("global physical memory regions table not initialized")]
    NotInitialized,
    /// The requested lock could not be acquired immediately.
    #[error("global physical memory regions table already locked")]
    AlreadyLocked,
}

/// Acquires a shared read lock on the global physical memory regions table and runs a reader while
/// holding it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table. Some operations, such as console output or logging, may
/// invoke address translation and must be checked carefully.
///
/// # Panics
///
/// Panics before early memory discovery initializes the table.
pub fn lock_read_phys_mem_regions<R>(reader: impl FnOnce(&[PhysMemoryRegion]) -> R) -> R {
    read_region_table(&PHYS_MEM_REGIONS, |table| reader(table.as_slice()))
}

/// Tries to acquire a shared read lock on the global physical memory regions table and run a reader
/// while holding it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table.
///
/// # Errors
///
/// Returns an [`PhysMemoryRegionsLockError`] if the global physical memory regions table has not
/// been initialized yet or if the lock could not be acquired.
pub fn try_lock_read_phys_mem_regions<R>(
    reader: impl FnOnce(&[PhysMemoryRegion]) -> R,
) -> Result<R, PhysMemoryRegionsLockError> {
    let regions = PHYS_MEM_REGIONS
        .get()
        .ok_or(PhysMemoryRegionsLockError::NotInitialized)?;
    try_read_region_table(regions, |table| reader(table.as_slice()))
        .ok_or(PhysMemoryRegionsLockError::AlreadyLocked)
}

/// Acquires an exclusive write lock on the global physical memory regions table and runs a writer
/// while holding it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table. Some operations, such as console output or logging, may
/// invoke address translation and must be checked carefully.
///
/// # Safety
///
/// The caller must preserve the region invariants: regions must remain sorted and
/// non-overlapping, every range must be nonempty, and all flags must remain valid.
/// Valid flags contain only defined bits and exactly one role.
///
/// # Panics
///
/// Panics before early memory discovery initializes the table.
pub unsafe fn lock_write_phys_mem_regions<R>(
    writer: impl FnOnce(&mut [PhysMemoryRegion]) -> R,
) -> R {
    // SAFETY: The caller promises to maintain the invariant.
    write_region_table(&PHYS_MEM_REGIONS, |table| {
        writer(unsafe { table.as_mut_slice() })
    })
}

/// Tries to acquire an exclusive write lock on the global physical memory regions table and run a
/// writer while holding it.
///
/// Local interrupts and preemption are disabled throughout the callback. The callback must not
/// acquire another lock on the same table. Some operations, such as console output or logging, may
/// invoke address translation and must be checked carefully.
///
/// # Errors
///
/// Returns an [`PhysMemoryRegionsLockError`] if the global physical memory regions table has not
/// been initialized yet or if the lock could not be acquired.
///
/// # Safety
///
/// The caller must preserve the region invariants: regions must remain sorted and
/// non-overlapping, every range must be nonempty, and all flags must remain valid.
/// Valid flags contain only defined bits and exactly one role.
pub unsafe fn try_lock_write_phys_mem_regions<R>(
    writer: impl FnOnce(&mut [PhysMemoryRegion]) -> R,
) -> Result<R, PhysMemoryRegionsLockError> {
    let regions = PHYS_MEM_REGIONS
        .get()
        .ok_or(PhysMemoryRegionsLockError::NotInitialized)?;
    // SAFETY: The caller promises to maintain the invariant.
    try_write_region_table(regions, |table| writer(unsafe { table.as_mut_slice() }))
        .ok_or(PhysMemoryRegionsLockError::AlreadyLocked)
}

/// Switches the global physical memory regions table to dynamically extensible storage.
///
/// Call after heap initialization and direct-map activation, before removing identity
/// mappings. The caller must exclude concurrent table updates and other storage switches
/// until this call returns.
///
/// Does nothing if the table already uses final storage.
///
/// # Errors
///
/// Returns [`PhysMemoryRegionsLockError::NotInitialized`] if the table is unavailable,
/// or [`PhysMemoryRegionsLockError::AlreadyLocked`] if either required lock cannot be
/// acquired immediately.
pub(super) fn switch_to_final_storage() -> Result<(), PhysMemoryRegionsLockError> {
    let regions = PHYS_MEM_REGIONS.get().ok_or(PhysMemoryRegionsLockError::NotInitialized)?;

    let early = try_read_region_table(regions, |table| match table {
        RuntimeRegions::Early(regions) => Some(regions.clone()),
        RuntimeRegions::Final(_) => None,
    }).ok_or(PhysMemoryRegionsLockError::AlreadyLocked)?;

    if let Some(early_regions) = early {
        // The conversion allocates, so do it without holding the ownership-table lock.
        let mut final_regions: FinalRegions = early_regions.into();
        relocate_region_descriptions(&mut final_regions);
        try_write_region_table(regions, |table| {
            *table = RuntimeRegions::Final(final_regions);
        }).ok_or(PhysMemoryRegionsLockError::AlreadyLocked)?;
    }

    Ok(())
}

/// Refreshes early kernel string references before their low identity mappings are removed.
///
/// Region descriptions are kernel static strings captured before the high-half jump. Copying the
/// region entries does not relocate these runtime pointers automatically.
pub(super) fn relocate_region_descriptions(regions: &mut FinalRegions) {
    // SAFETY: We update the region descriptions in place, without changing the ranges or attributes.
    for region in unsafe { regions.iter_mut() } {
        let address = VirtAddr::from_ptr_of(region.desc.as_ptr());
        let desc = if vmm::direct_mapping_virt_to_phys(address).is_some() {
            region.desc
        } else {
            let high = vmm::direct_mapping_phys_to_virt(PhysAddr::from_usize(address.as_usize()))
                .expect("early region description exceeds the direct map");
            // SAFETY: Early descriptions are immutable kernel static strings. Both aliases
            // currently map the same read-only kernel bytes, so length and UTF-8 are unchanged
            // and the high alias remains valid after the identity alias is removed.
            unsafe {
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                    high.as_ptr(),
                    region.desc.len(),
                ))
            }
        };
        region.desc = desc;
    }
}

/// Clones the global physical memory regions table if it's using the final storage.
///
/// The returned copy can be modified outside the table lock and installed with
/// [`replace_final_regions`].
///
/// # Panics
///
/// Panics if the global physical memory regions table is not initialized or is using the early
/// storage.
pub(super) fn clone_final_regions() -> FinalRegions {
    read_region_table(&PHYS_MEM_REGIONS, |table| {
        let RuntimeRegions::Final(regions) = table else {
            panic!("early physical region storage is still in use");
        };
        regions.clone()
    })
}

/// Replaces the global physical memory regions table with a new set of regions, returning the
/// previous one.
///
/// Replaces the table under an exclusive lock. Callers preparing a replacement from
/// [`clone_final_regions`] must coordinate the complete read-modify-replace operation
/// with other writers to avoid overwriting concurrent changes.
///
/// # Panics
///
/// Panics if the global physical memory regions table is not initialized or is using the early
/// storage.
pub(super) fn replace_final_regions(regions: FinalRegions) -> FinalRegions {
    let previous = write_region_table(&PHYS_MEM_REGIONS, |table| {
        let RuntimeRegions::Final(current) = table else {
            panic!("physical region storage is still fixed");
        };
        core::mem::replace(current, regions)
    });
    previous
}

/// Converts an identity-mapped kernel section range to its physical range.
fn virt_range_to_id_phys_range(range: VirtAddrRange) -> PhysAddrRange {
    PhysAddrRange::new(
        PhysAddr::from_usize(range.start.as_usize()),
        PhysAddr::from_usize(range.end.as_usize()),
    )
}

/// Initializes the global physical memory regions table with the memory regions reported by the
/// platform, the kernel sections, and the loader range. The table is initialized with the early
/// storage.
///
/// Kernel and loader ranges are installed first. Platform reservations and boot data must not
/// overlap them. Free RAM fills only gaps left by those occupied regions.
///
/// # Panics
///
/// Panics if the global physical memory regions table has already been initialized,
/// kernel or loader regions are invalid or overlap, platform reservations conflict
/// with existing regions, or the resulting table exceeds the early storage capacity.
pub(super) fn init_early_phys_mem_regions(raw: &RawMemoryRegions, loader_range: PhysAddrRange) {
    let mut regions = EarlyRegions::new();

    // Insert the kernel sections first.
    for section in sections::all_sections() {
        if section.aligned_range.is_empty() {
            continue;
        }
        regions
            .insert_disjoint(PhysMemoryRegion {
                range: virt_range_to_id_phys_range(section.aligned_range),
                flags: section.flags | MemoryRegionFlags::KERNEL,
                desc: section.name,
            })
            .expect("invalid or overlapping kernel memory section");
    }

    // Insert the loader range next.
    regions
        .insert_disjoint(PhysMemoryRegion {
            range: loader_range,
            flags: MemoryRegionFlags::BOOT_SERVICE
                | MemoryRegionFlags::READ
                | MemoryRegionFlags::WRITE,
            desc: "loader",
        })
        .expect("invalid or overlapping loader memory region");

    // Insert the non-free platform-reported memory regions next.
    for region in raw {
        if !region.flags.contains(MemoryRegionFlags::FREE) {
            regions
                .insert_disjoint(*region)
                .expect("platform reservation conflicts with kernel or loader memory");
        }
    }

    // Insert the free platform-reported memory regions last.
    for region in raw {
        if region.flags.contains(MemoryRegionFlags::FREE) {
            regions
                .insert_uncovered(*region)
                .expect("too many physical memory regions");
        }
    }

    // Initialize the physical memory regions table.
    PHYS_MEM_REGIONS.init_once(RegionTable::new(RuntimeRegions::Early(regions)));
}
