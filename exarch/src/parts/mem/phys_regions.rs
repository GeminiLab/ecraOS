//! Sorted physical memory region collections.

use alloc::vec::Vec;
use core::{fmt, ops::Index, slice};

use heapless::Vec as HeaplessVec;
use memory_addr::{PhysAddr, PhysAddrRange};

use super::{MemoryRegionFlags, PhysMemoryRegion};

#[cfg(test)]
mod tests;

/// A failure to validate or update physical memory regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysMemoryRegionsError {
    /// An address range is empty, reversed, or changed by a transformation.
    InvalidRange,
    /// Flags contain unknown bits or do not have exactly one ownership role.
    InvalidFlags,
    /// An insertion overlaps an existing region.
    Overlap,
    /// An overwrite predicate rejected an existing intersection.
    OverwriteRejected,
    /// The result does not fit in fixed-capacity storage.
    CapacityExceeded,
    /// Dynamic storage could not allocate the required capacity.
    AllocationFailed,
}

/// Storage primitives for a physical region collection.
///
/// Default must create empty storage. Clone preserves entries and uses the storage type's normal
/// allocation-failure behavior. Insertion preserves existing element order. A failed
/// reservation or insertion must preserve the contents. Mutable access through the
/// collection's unsafe APIs requires callers to preserve its range and flag invariants.
pub trait PhysMemoryRegionsStorage: Default + Clone {
    /// Returns all stored regions as an immutable slice in order.
    fn as_slice(&self) -> &[PhysMemoryRegion];

    /// Returns all stored regions as a mutable slice in order.
    fn as_mut_slice(&mut self) -> &mut [PhysMemoryRegion];

    /// Reserves capacity for at least the given total number of elements.
    ///
    /// The count includes existing entries, rather than specifying additional entries. A count
    /// below the current length succeeds without shrinking storage. The contents are unchanged.
    fn reserve_regions(&mut self, total: usize) -> Result<(), PhysMemoryRegionsError>;

    /// Inserts an element at a valid index without reordering other elements.
    ///
    /// The index must not exceed the current length. A successful reservation for the new length
    /// guarantees that this operation succeeds without allocating.
    fn insert_region(
        &mut self,
        index: usize,
        region: PhysMemoryRegion,
    ) -> Result<(), PhysMemoryRegionsError>;
}

/// Allocation-free storage operations on an inline-capacity vector.
impl<const N: usize> PhysMemoryRegionsStorage for HeaplessVec<PhysMemoryRegion, N> {
    /// Returns the live inline entries.
    fn as_slice(&self) -> &[PhysMemoryRegion] {
        self.as_ref()
    }

    /// Returns the live inline entries.
    fn as_mut_slice(&mut self) -> &mut [PhysMemoryRegion] {
        self.as_mut()
    }

    /// Rejects a requested total greater than the inline capacity.
    fn reserve_regions(&mut self, total: usize) -> Result<(), PhysMemoryRegionsError> {
        if total <= N {
            Ok(())
        } else {
            Err(PhysMemoryRegionsError::CapacityExceeded)
        }
    }

    /// Inserts an inline entry and reports capacity exhaustion.
    fn insert_region(
        &mut self,
        index: usize,
        region: PhysMemoryRegion,
    ) -> Result<(), PhysMemoryRegionsError> {
        self.insert(index, region)
            .map_err(|_| PhysMemoryRegionsError::CapacityExceeded)
    }
}

/// Fallible storage operations on a dynamically allocated vector.
impl PhysMemoryRegionsStorage for Vec<PhysMemoryRegion> {
    /// Returns the allocated entries.
    fn as_slice(&self) -> &[PhysMemoryRegion] {
        self
    }

    /// Returns the allocated entries.
    fn as_mut_slice(&mut self) -> &mut [PhysMemoryRegion] {
        self
    }

    /// Grows the allocation to fit the requested total number of entries.
    fn reserve_regions(&mut self, total: usize) -> Result<(), PhysMemoryRegionsError> {
        // Vec takes an additional count, while this trait takes the final total count.
        self.try_reserve(total.saturating_sub(self.len()))
            .map_err(|_| PhysMemoryRegionsError::AllocationFailed)
    }

    /// Inserts an entry after fallibly reserving space if necessary.
    fn insert_region(
        &mut self,
        index: usize,
        region: PhysMemoryRegion,
    ) -> Result<(), PhysMemoryRegionsError> {
        self.reserve_regions(self.len() + 1)?;
        self.insert(index, region);
        Ok(())
    }
}

/// A sorted collection of nonempty, nonoverlapping regions with one ownership role each.
///
/// Ranges retain byte precision. Adjacent regions are not merged. Every fallible mutation keeps
/// the original contents on error. Callbacks are evaluated at most once per visited intersection
/// and should not perform external mutations if their effects would need to be rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysMemoryRegions<S: PhysMemoryRegionsStorage>(S);

/// Empty collection construction for any supported storage.
impl<S: PhysMemoryRegionsStorage> Default for PhysMemoryRegions<S> {
    /// Creates an empty collection in the selected storage.
    fn default() -> Self {
        Self::new()
    }
}

/// Validated queries and atomic range updates over interchangeable storage.
impl<S: PhysMemoryRegionsStorage> PhysMemoryRegions<S> {
    /// Creates an empty region collection.
    pub fn new() -> Self {
        Self(S::default())
    }

    /// Returns the regions without permitting mutation of their ranges or attributes.
    pub fn as_slice(&self) -> &[PhysMemoryRegion] {
        self.0.as_slice()
    }

    /// Returns the regions as a mutable slice.
    ///
    /// # Safety
    ///
    /// The caller must preserve the region invariants: regions must remain sorted and
    /// non-overlapping, every range must be nonempty, and all flags must remain valid.
    /// Valid flags contain only defined bits and exactly one role.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [PhysMemoryRegion] {
        self.0.as_mut_slice()
    }

    /// Returns an iterator in ascending address order.
    pub fn iter(&self) -> slice::Iter<'_, PhysMemoryRegion> {
        self.as_slice().iter()
    }

    /// Returns an iterator in ascending address order that permits mutation of the regions.
    ///
    /// # Safety
    ///
    /// The caller must preserve the region invariants: regions must remain sorted and
    /// non-overlapping, every range must be nonempty, and all flags must remain valid.
    /// Valid flags contain only defined bits and exactly one role.
    pub unsafe fn iter_mut(&mut self) -> slice::IterMut<'_, PhysMemoryRegion> {
        // SAFETY: The caller promises to maintain the invariant.
        unsafe { self.as_mut_slice().iter_mut() }
    }

    /// Returns the number of stored regions.
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Returns whether no regions are stored.
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    /// Consumes the collection and returns its storage.
    pub fn into_storage(self) -> S {
        self.0
    }

    /// Finds the region containing an address, or returns None for an uncovered address.
    pub fn find(&self, address: PhysAddr) -> Option<&PhysMemoryRegion> {
        let index = self
            .as_slice()
            .partition_point(|region| region.range.end <= address);
        self.as_slice()
            .get(index)
            .filter(|region| region.range.start <= address)
    }

    /// Iterates over regions intersecting a range, preserving their full stored ranges.
    pub fn overlaps(&self, range: PhysAddrRange) -> impl Iterator<Item = &PhysMemoryRegion> {
        let start = self
            .as_slice()
            .partition_point(|region| region.range.end <= range.start);
        self.as_slice()[start..]
            .iter()
            .take_while(move |region| region.range.start < range.end)
            .filter(move |_| range.start < range.end)
    }

    /// Iterates over uncovered portions of a byte range in address order.
    ///
    /// An empty or reversed input produces no portions. The iterator does not allocate.
    pub fn uncovered(&self, range: PhysAddrRange) -> impl Iterator<Item = PhysAddrRange> + '_ {
        let mut cursor = range.start;
        let mut existing = self.overlaps(range);
        core::iter::from_fn(move || {
            while cursor < range.end {
                let Some(next) = existing.next() else {
                    let gap = PhysAddrRange::new(cursor, range.end);
                    cursor = range.end;
                    return Some(gap);
                };
                let start = cursor;
                cursor = cursor.max(next.range.end);
                if start < next.range.start {
                    return Some(PhysAddrRange::new(start, next.range.start));
                }
            }
            None
        })
    }

    /// Inserts a complete region and rejects any existing overlap.
    ///
    /// # Errors
    ///
    /// Returns InvalidRange or InvalidFlags for invalid input, Overlap for an intersection, and
    /// a storage error if the insertion cannot fit. No error changes the collection contents.
    pub fn insert_disjoint(
        &mut self,
        region: PhysMemoryRegion,
    ) -> Result<(), PhysMemoryRegionsError> {
        validate_region(region)?;
        let index = self
            .as_slice()
            .partition_point(|other| other.range.start < region.range.start);
        if (index > 0 && self[index - 1].range.end > region.range.start)
            || (index < self.len() && self[index].range.start < region.range.end)
        {
            return Err(PhysMemoryRegionsError::Overlap);
        }
        self.0.reserve_regions(self.len() + 1)?;
        self.0.insert_region(index, region)
    }

    /// Adds only the uncovered portions of a region, retaining all existing regions.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input or insufficient storage. No error changes the
    /// collection contents, including when the range splits into many uncovered pieces.
    pub fn insert_uncovered(
        &mut self,
        region: PhysMemoryRegion,
    ) -> Result<(), PhysMemoryRegionsError> {
        validate_region(region)?;
        let mut staged = S::default();
        let mut cursor = region.range.start;
        for existing in self.iter().copied() {
            if cursor < region.range.end && existing.range.start > cursor {
                let end = existing.range.start.min(region.range.end);
                append(&mut staged, clipped(region, cursor, end))?;
            }
            append(&mut staged, existing)?;
            if existing.range.end > cursor {
                cursor = existing.range.end;
            }
        }
        if cursor < region.range.end {
            append(&mut staged, clipped(region, cursor, region.range.end))?;
        }
        self.0 = staged;
        Ok(())
    }

    /// Overwrites a complete incoming range if every existing intersection is allowed.
    ///
    /// Each predicate argument retains the old flags and description but is clipped to its exact
    /// intersection with the incoming range. The predicate is called once per intersection until
    /// one is rejected. Gaps are filled with the incoming region. Old portions outside the range
    /// retain their attributes.
    ///
    /// # Errors
    ///
    /// Returns OverwriteRejected when a predicate refuses any overlap, or an input or storage
    /// error before committing the replacement.
    pub fn insert_overwrite_if<F>(
        &mut self,
        region: PhysMemoryRegion,
        mut allow: F,
    ) -> Result<(), PhysMemoryRegionsError>
    where
        F: FnMut(PhysMemoryRegion) -> bool,
    {
        validate_region(region)?;
        for old in self.overlaps(region.range).copied() {
            let overlap = intersection(old, region.range);
            if !allow(overlap) {
                return Err(PhysMemoryRegionsError::OverwriteRejected);
            }
        }
        let mut staged = S::default();
        let mut inserted = false;
        for old in self.iter().copied() {
            if old.range.end <= region.range.start {
                append(&mut staged, old)?;
                continue;
            }
            if old.range.start < region.range.start {
                append(
                    &mut staged,
                    clipped(old, old.range.start, region.range.start),
                )?;
            }
            if !inserted {
                append(&mut staged, region)?;
                inserted = true;
            }
            if old.range.start >= region.range.end {
                append(&mut staged, old)?;
            } else if old.range.end > region.range.end {
                append(&mut staged, clipped(old, region.range.end, old.range.end))?;
            }
        }
        if !inserted {
            append(&mut staged, region)?;
        }
        self.0 = staged;
        Ok(())
    }

    /// Transforms only existing intersections without filling any gaps.
    ///
    /// Each callback argument is an old region clipped to the requested range. The returned
    /// region must retain that exact range and contain valid flags. Portions outside the range
    /// are preserved.
    ///
    /// # Errors
    ///
    /// Invalid input, invalid callback results, and storage failures leave the contents unchanged.
    pub fn transform_overlaps<F>(
        &mut self,
        range: PhysAddrRange,
        transform: F,
    ) -> Result<(), PhysMemoryRegionsError>
    where
        F: FnMut(PhysMemoryRegion) -> PhysMemoryRegion,
    {
        self.rewrite(range, None, transform)
    }

    /// Transforms existing intersections and fills gaps from an incoming region atomically.
    ///
    /// The callback has the same contract as transform_overlaps. Uncovered parts receive the
    /// incoming flags and description. This combines preserving old attributes with recording a
    /// complete reservation.
    ///
    /// # Errors
    ///
    /// Invalid input, invalid callback results, and storage failures leave the contents unchanged.
    pub fn overlay_with<F>(
        &mut self,
        region: PhysMemoryRegion,
        transform: F,
    ) -> Result<(), PhysMemoryRegionsError>
    where
        F: FnMut(PhysMemoryRegion) -> PhysMemoryRegion,
    {
        validate_region(region)?;
        self.rewrite(region.range, Some(region), transform)
    }

    /// Rebuilds a changed range in separate storage before committing its contents.
    fn rewrite<F>(
        &mut self,
        range: PhysAddrRange,
        fill: Option<PhysMemoryRegion>,
        mut transform: F,
    ) -> Result<(), PhysMemoryRegionsError>
    where
        F: FnMut(PhysMemoryRegion) -> PhysMemoryRegion,
    {
        validate_range(range)?;
        let mut staged = S::default();
        let mut cursor = range.start;
        for old in self.iter().copied() {
            if old.range.end <= range.start {
                append(&mut staged, old)?;
                continue;
            }
            if old.range.start >= range.end {
                if let Some(fill) = fill
                    && cursor < range.end
                {
                    append(&mut staged, clipped(fill, cursor, range.end))?;
                }
                cursor = range.end;
                append(&mut staged, old)?;
                continue;
            }
            let overlap = intersection(old, range);
            if old.range.start < overlap.range.start {
                append(
                    &mut staged,
                    clipped(old, old.range.start, overlap.range.start),
                )?;
            }
            if let Some(fill) = fill
                && cursor < overlap.range.start
            {
                append(&mut staged, clipped(fill, cursor, overlap.range.start))?;
            }
            let changed = transform(overlap);
            validate_region(changed)?;
            if changed.range != overlap.range {
                return Err(PhysMemoryRegionsError::InvalidRange);
            }
            append(&mut staged, changed)?;
            cursor = overlap.range.end;
            if overlap.range.end < old.range.end {
                append(&mut staged, clipped(old, overlap.range.end, old.range.end))?;
            }
        }
        if let Some(fill) = fill
            && cursor < range.end
        {
            append(&mut staged, clipped(fill, cursor, range.end))?;
        }
        self.0 = staged;
        Ok(())
    }
}

/// One-way handoff from inline boot storage to dynamically allocated storage.
impl<const N: usize> From<PhysMemoryRegions<HeaplessVec<PhysMemoryRegion, N>>>
    for PhysMemoryRegions<Vec<PhysMemoryRegion>>
{
    /// Converts inline entries to dynamic storage after an allocator is available.
    fn from(regions: PhysMemoryRegions<HeaplessVec<PhysMemoryRegion, N>>) -> Self {
        // Copy just the live entries instead of moving an inline-capacity owning iterator through
        // several collection helpers on the small boot stack.
        Self(regions.as_slice().to_vec())
    }
}

/// Immutable borrowing of the region entries.
impl<S: PhysMemoryRegionsStorage> AsRef<[PhysMemoryRegion]> for PhysMemoryRegions<S> {
    /// Borrows the stored entries without allowing invariant-breaking mutations.
    fn as_ref(&self) -> &[PhysMemoryRegion] {
        self.as_slice()
    }
}

/// Immutable indexing without exposing mutable region entries.
impl<S: PhysMemoryRegionsStorage> Index<usize> for PhysMemoryRegions<S> {
    /// The immutable region selected by an index.
    type Output = PhysMemoryRegion;

    /// Returns an entry by its sorted index.
    ///
    /// # Panics
    ///
    /// Panics if the index is out of bounds.
    fn index(&self, index: usize) -> &Self::Output {
        &self.as_slice()[index]
    }
}

/// Borrowed iteration over the physical-address ordering.
impl<'a, S: PhysMemoryRegionsStorage> IntoIterator for &'a PhysMemoryRegions<S> {
    /// An immutable reference to an ownership region.
    type Item = &'a PhysMemoryRegion;
    /// The region iterator in physical address order.
    type IntoIter = slice::Iter<'a, PhysMemoryRegion>;

    /// Borrows entries in their stored address order.
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Rejects empty or reversed address ranges.
fn validate_range(range: PhysAddrRange) -> Result<(), PhysMemoryRegionsError> {
    if range.start >= range.end {
        Err(PhysMemoryRegionsError::InvalidRange)
    } else {
        Ok(())
    }
}

/// Checks byte-range validity and architecture-independent flag invariants.
fn validate_region(region: PhysMemoryRegion) -> Result<(), PhysMemoryRegionsError> {
    validate_range(region.range)?;
    if !region.flags.role_sanity_check()
        || region.flags.bits() & !MemoryRegionFlags::all().bits() != 0
    {
        Err(PhysMemoryRegionsError::InvalidFlags)
    } else {
        Ok(())
    }
}

/// Copies an existing region's attributes onto a contained byte range.
fn clipped(region: PhysMemoryRegion, start: PhysAddr, end: PhysAddr) -> PhysMemoryRegion {
    PhysMemoryRegion {
        range: PhysAddrRange::new(start, end),
        ..region
    }
}

/// Clips a region to a known nonempty intersection.
fn intersection(region: PhysMemoryRegion, range: PhysAddrRange) -> PhysMemoryRegion {
    clipped(
        region,
        region.range.start.max(range.start),
        region.range.end.min(range.end),
    )
}

/// Appends a result segment while checking its order against the previous segment.
fn append<S: PhysMemoryRegionsStorage>(
    storage: &mut S,
    region: PhysMemoryRegion,
) -> Result<(), PhysMemoryRegionsError> {
    if storage
        .as_slice()
        .last()
        .is_some_and(|last| last.range.end > region.range.start)
    {
        return Err(PhysMemoryRegionsError::Overlap);
    }
    storage.insert_region(storage.as_slice().len(), region)
}

/// Named error formatting for boot-time diagnostics.
impl fmt::Display for PhysMemoryRegionsError {
    /// Formats the named validation or storage failure.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
