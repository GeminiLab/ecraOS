#![doc = include_str!("../README.md")]
#![no_std]
#![feature(allocator_api)]
#![feature(btree_cursors)]
#![feature(btreemap_alloc)]

extern crate alloc;

use alloc::{
    alloc::{Allocator, Global},
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
};
use core::{
    cmp::Ordering,
    fmt,
    ops::{Bound, Deref},
};

use memory_addr::{AddrRange, AddrRangeBounds, MemoryAddr};

/// Tests for range set behavior.
///
/// Defines behavior specifications for range insertion, removal, lookup, and free-range queries.
#[cfg(test)]
mod tests;

/// An occupied range stored in a range set.
///
/// This entry keeps the occupied address range together with the payload associated with that
/// range.
struct RangeInSet<M: MemoryAddr, P> {
    /// The occupied address range.
    ///
    /// Its start address is also the key of this entry in the occupied-range map.
    range: AddrRange<M>,
    /// The payload associated with the range.
    ///
    /// It is returned to the caller when the range is removed.
    payload: P,
}

/// An address range ordered by its size.
///
/// This wrapper lets the free-range size index search for the smallest range that satisfies a
/// requested allocation size.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(transparent)]
struct AddrRangeBySize<M: MemoryAddr>(pub AddrRange<M>);

/// Constructs size-ordered address ranges.
///
/// The constructors create stored index entries and lookup sentinels.
impl<M: MemoryAddr> AddrRangeBySize<M> {
    /// Creates a size-ordered address range.
    ///
    /// Wraps the given address range so it can be stored in the size-ordered free-range index.
    const fn new(range: AddrRange<M>) -> Self {
        Self(range)
    }

    /// Creates a lower dummy size-ordered address range.
    ///
    /// Builds a sentinel that is less than or equal to any other size-ordered range with the same
    /// size.
    fn dummy_lower(size: usize) -> Self {
        // SAFETY: We know that the start and end are valid addresses.
        unsafe { Self(AddrRange::new_unchecked(0.into(), size.into())) }
    }
}

/// Provides immutable access to the wrapped range of a size-ordered entry.
///
/// Range accessors such as `size` and `contains` can be called on the wrapper directly.
impl<M: MemoryAddr> Deref for AddrRangeBySize<M> {
    /// The wrapped address range type.
    ///
    /// The wrapper changes only the ordering of this range.
    type Target = AddrRange<M>;

    /// Dereferences to the wrapped address range.
    ///
    /// Allows size-ordered range wrappers to be used where immutable address range access is
    /// needed.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Partially orders size-ordered address ranges.
///
/// The ordering is total and delegates to [`Ord`].
impl<M: MemoryAddr> PartialOrd for AddrRangeBySize<M> {
    /// Partially compares ranges by size, then start address.
    ///
    /// Provides the same ordering keys as [`Ord`] while preserving the trait contract.
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Orders address ranges by size, then start address.
///
/// The smallest suitable range is therefore the first entry at or above a size sentinel.
impl<M: MemoryAddr> Ord for AddrRangeBySize<M> {
    /// Compares ranges by size, then start address.
    ///
    /// Orders free ranges for best-fit lookup with deterministic tie-breaking by address.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match self.size().cmp(&other.size()) {
            Ordering::Equal => self.0.start.cmp(&other.0.start),
            other => other,
        }
    }
}

/// An address range ordered by its start address.
///
/// This wrapper lets the free-range address index search for ranges by their position in the
/// managed address space.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(transparent)]
struct AddrRangeByStart<M: MemoryAddr>(pub AddrRange<M>);

/// Constructs start-ordered address ranges.
///
/// The constructors create stored index entries and lookup sentinels.
impl<M: MemoryAddr> AddrRangeByStart<M> {
    /// Creates a start-ordered address range.
    ///
    /// Wraps the given address range so it can be stored in the address-ordered free-range index.
    const fn new(range: AddrRange<M>) -> Self {
        Self(range)
    }

    /// Creates a lower dummy start-ordered address range.
    ///
    /// Builds a sentinel that is less than or equal to any other start-ordered range with the same
    /// start address.
    fn dummy_lower(addr: M) -> Self {
        // SAFETY: We know that the start and end are valid addresses.
        unsafe { Self(AddrRange::new_unchecked(addr, addr)) }
    }
}

/// Provides immutable access to the wrapped range of a start-ordered entry.
///
/// Range accessors such as `start` and `contains` can be called on the wrapper directly.
impl<M: MemoryAddr> Deref for AddrRangeByStart<M> {
    /// The wrapped address range type.
    ///
    /// The wrapper changes only the ordering of this range.
    type Target = AddrRange<M>;

    /// Dereferences to the wrapped address range.
    ///
    /// Allows start-ordered range wrappers to be used where immutable address range access is
    /// needed.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Partially orders start-ordered address ranges.
///
/// The ordering is total and delegates to [`Ord`].
impl<M: MemoryAddr> PartialOrd for AddrRangeByStart<M> {
    /// Partially compares ranges by start address, then end address.
    ///
    /// Provides the same ordering keys as [`Ord`] while preserving the trait contract.
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Orders address ranges by start address, then end address.
///
/// The free range containing an address is therefore the last entry that starts at or below it.
impl<M: MemoryAddr> Ord for AddrRangeByStart<M> {
    /// Compares ranges by start address, then end address.
    ///
    /// Orders free ranges for address lookup with deterministic tie-breaking by range end.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match self.0.start.cmp(&other.0.start) {
            Ordering::Equal => self.0.end.cmp(&other.0.end),
            other => other,
        }
    }
}

/// An error returned by range insertion.
///
/// Describes why a range could not be inserted and returns the insertion payload to the caller.
///
/// # Examples
///
/// ```
/// use memory_addr::{VirtAddr, va_range};
/// use memory_range_set::{RangeSet, TryInsertError};
///
/// let mut set = RangeSet::new(va_range!(0..100));
/// set.try_insert(va_range!(20..40), "kernel")?;
///
/// assert_eq!(
///     set.try_insert(va_range!(30..50), "driver"),
///     Err(TryInsertError::Overlap {
///         payload: "driver",
///         existing: va_range!(20..40),
///     })
/// );
/// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryInsertError<M: MemoryAddr, P> {
    /// A range overlapping an existing range.
    ///
    /// Indicates that the requested insertion overlaps a range already present in the set.
    Overlap {
        /// The insertion payload.
        ///
        /// Returns the payload supplied for the failed insertion.
        payload: P,

        /// The overlapping existing range.
        ///
        /// Identifies the range already present in the set that conflicts with the insertion.
        existing: AddrRange<M>,
    },

    /// A range outside the set limit.
    ///
    /// Indicates that the requested insertion is not fully contained in the set limit.
    OutOfLimit {
        /// The insertion payload.
        ///
        /// Returns the payload supplied for the failed insertion.
        payload: P,

        /// The requested insertion range.
        ///
        /// Identifies the range that was rejected because it falls outside the set limit.
        range: AddrRange<M>,

        /// The range set limit.
        ///
        /// Identifies the containing limit required for all inserted ranges.
        limit: AddrRange<M>,
    },

    /// An empty range.
    ///
    /// Indicates that the requested insertion contains no address. An empty range would split a
    /// free range without occupying any of it, so it is rejected before any other check.
    Empty {
        /// The insertion payload.
        ///
        /// Returns the payload supplied for the failed insertion.
        payload: P,

        /// The requested insertion range.
        ///
        /// Its start and end addresses are equal.
        range: AddrRange<M>,
    },
}

/// Formats insertion errors as messages.
///
/// The messages describe the rejected or conflicting ranges and omit the payload.
impl<M: MemoryAddr + fmt::Debug, P> fmt::Display for TryInsertError<M, P> {
    /// Writes the error message.
    ///
    /// Ranges are written with their debug representation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap { existing, .. } => {
                write!(f, "range overlaps the existing range {existing:?}")
            }
            Self::OutOfLimit { range, limit, .. } => {
                write!(f, "range {range:?} is outside the limit {limit:?}")
            }
            Self::Empty { range, .. } => write!(f, "range {range:?} is empty"),
        }
    }
}

/// Reports insertion errors through the standard error trait.
///
/// The errors have no source.
impl<M: MemoryAddr + fmt::Debug, P: fmt::Debug> core::error::Error for TryInsertError<M, P> {}

/// A set of non-overlapping occupied memory ranges.
///
/// The set manages a fixed address limit. Inserted ranges are treated as occupied and are
/// associated with a payload, while the remaining portions of the limit are tracked as free ranges.
/// Occupied and free ranges are never empty.
///
/// `M` is the address type and `P` is the payload type. `A` is the allocator of the internal
/// B-tree nodes. [`RangeSet::new`] uses the global allocator, and [`RangeSet::new_in`] accepts
/// any cloneable allocator.
pub struct RangeSet<M: MemoryAddr, P = (), A: Allocator = Global>
where
    A: Clone,
{
    /// The address range that contains every occupied and free range.
    ///
    /// The limit is fixed when the set is created.
    limit: AddrRange<M>,
    /// The occupied ranges keyed by start address.
    ///
    /// Each entry stores the complete range and its payload.
    map: BTreeMap<M, RangeInSet<M, P>, A>,
    /// The free ranges ordered by size, then start address.
    ///
    /// This index serves best-fit lookup and contains the same ranges as `free_ranges_by_addr`.
    free_ranges_by_size: BTreeSet<AddrRangeBySize<M>, A>,
    /// The free ranges ordered by start address.
    ///
    /// This index serves address lookup and contains the same ranges as `free_ranges_by_size`.
    free_ranges_by_addr: BTreeSet<AddrRangeByStart<M>, A>,
}

/// An internal inconsistency panic helper.
///
/// Emits a uniform panic message for impossible states where the occupied-range map and free-range
/// indexes are no longer synchronized.
macro_rules! unreachable_inconsistency {
    () => {
        unreachable!("internal error: inconsistency between allocated ranges and free ranges");
    };
}

/// Creates range sets that use the global allocator.
///
/// Sets that use another allocator are created with [`RangeSet::new_in`].
impl<M: MemoryAddr, P> RangeSet<M, P> {
    /// Creates a range set with the given address limit.
    ///
    /// The new set starts with no occupied ranges and with the whole limit available as a free
    /// range. An empty limit has no free range.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let set = RangeSet::<VirtAddr>::new(va_range!(0..100));
    /// assert_eq!(set.limit(), va_range!(0..100));
    /// assert_eq!(set.find_free_range(100), Some(va_range!(0..100)));
    /// ```
    pub fn new(limit: AddrRange<M>) -> Self {
        Self::new_in(limit, Global)
    }
}

/// Creates, queries, and updates range sets.
///
/// Every update keeps the occupied-range map and both free-range indexes consistent.
impl<M: MemoryAddr, P, A: Allocator + Clone> RangeSet<M, P, A> {
    /// Creates a range set with the given address limit and allocator.
    ///
    /// The internal B-tree nodes are allocated with clones of `alloc`. Apart from the allocator,
    /// the new set is the same as one created by [`RangeSet::new`].
    ///
    /// # Examples
    ///
    /// ```
    /// #![feature(allocator_api)]
    ///
    /// use std::alloc::System;
    ///
    /// use memory_addr::{VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let set = RangeSet::<VirtAddr, (), _>::new_in(va_range!(0..100), System);
    /// assert_eq!(set.find_free_range(100), Some(va_range!(0..100)));
    /// ```
    pub fn new_in(limit: AddrRange<M>, alloc: A) -> Self {
        let mut result = Self {
            limit,
            map: BTreeMap::new_in(alloc.clone()),
            free_ranges_by_size: BTreeSet::new_in(alloc.clone()),
            free_ranges_by_addr: BTreeSet::new_in(alloc),
        };

        if !limit.is_empty() {
            result.insert_free_range(limit);
        }

        result
    }

    /// Finds the range containing the given address.
    ///
    /// Returns the occupied range and an immutable reference to its payload, or `None` when the
    /// address is free or outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), "kernel")?;
    /// assert_eq!(set.find(va!(30)).map(|(_, payload)| *payload), Some("kernel"));
    /// assert_eq!(set.find(va!(50)), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn find(&self, addr: M) -> Option<(AddrRange<M>, &P)> {
        let (_, candidate) = self.map.range(..=addr).next_back()?;
        candidate
            .range
            .contains(addr)
            .then_some((candidate.range, &candidate.payload))
    }

    /// Finds the range containing the given address mutably.
    ///
    /// Returns the occupied range and a mutable reference to its payload, or `None` when the
    /// address is free or outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), 1)?;
    /// if let Some((_, payload)) = set.find_mut(va!(30)) {
    ///     *payload = 2;
    /// }
    /// assert_eq!(set.find(va!(30)).map(|(_, payload)| *payload), Some(2));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, i32>>(())
    /// ```
    pub fn find_mut(&mut self, addr: M) -> Option<(AddrRange<M>, &mut P)> {
        let (_, candidate) = self.map.range_mut(..=addr).next_back()?;
        candidate
            .range
            .contains(addr)
            .then_some((candidate.range, &mut candidate.payload))
    }

    /// Removes the range containing the given address.
    ///
    /// Returns the removed range and its payload, or `None` when the address is free or outside the
    /// set limit. The removed range becomes free and is coalesced with neighboring free ranges.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), "kernel")?;
    /// assert_eq!(
    ///     set.remove(va!(30)),
    ///     Some((va_range!(20..40), "kernel"))
    /// );
    /// assert_eq!(set.find_free_range_at(va!(30)), Some(va_range!(0..100)));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn remove(&mut self, addr: M) -> Option<(AddrRange<M>, P)> {
        let (&start, candidate) = self.map.range(..=addr).next_back()?;

        if !candidate.range.contains(addr) {
            return None;
        }

        let value = self.map.remove(&start)?;
        let mut merged = value.range;

        if let Some(left) = self.find_free_range_ending_at(merged.start) {
            self.remove_free_range(left);
            merged = AddrRange::new(left.start, merged.end);
        }

        if let Some(right) = self.find_free_range_starting_at(merged.end) {
            self.remove_free_range(right);
            merged = AddrRange::new(merged.start, right.end);
        }

        self.insert_free_range(merged);
        Some((value.range, value.payload))
    }

    /// Removes a free range from both free-range indexes.
    ///
    /// Panics with an internal inconsistency if either index does not contain the range.
    fn remove_free_range(&mut self, range: AddrRange<M>) {
        if !self
            .free_ranges_by_addr
            .remove(&AddrRangeByStart::new(range))
        {
            unreachable_inconsistency!();
        }
        if !self
            .free_ranges_by_size
            .remove(&AddrRangeBySize::new(range))
        {
            unreachable_inconsistency!();
        }
    }

    /// Inserts a free range into both free-range indexes.
    ///
    /// Keeps the address-ordered and size-ordered free-range indexes synchronized.
    fn insert_free_range(&mut self, range: AddrRange<M>) {
        self.free_ranges_by_addr
            .insert(AddrRangeByStart::new(range));
        self.free_ranges_by_size.insert(AddrRangeBySize::new(range));
    }

    /// Tries to insert a range into the set.
    ///
    /// Inserts the range as occupied and associates it with the payload. Returns an error with the
    /// payload when the range is empty, falls outside the set limit, or overlaps an existing
    /// occupied range. The checks run in that order.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), "kernel")?;
    /// assert_eq!(set.find(va!(30)).map(|(_, payload)| *payload), Some("kernel"));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn try_insert(
        &mut self,
        range: AddrRange<M>,
        payload: P,
    ) -> Result<(), TryInsertError<M, P>> {
        if range.is_empty() {
            return Err(TryInsertError::Empty { payload, range });
        }

        if !self.limit.contains_range(range) {
            return Err(TryInsertError::OutOfLimit {
                payload,
                range,
                limit: self.limit,
            });
        }

        let cursor = self.map.lower_bound(Bound::Excluded(&range.start));

        // Check whether the previous range overlaps with the new range.
        if let Some((_, candidate)) = cursor.peek_prev()
            && candidate.range.end > range.start
        {
            return Err(TryInsertError::Overlap {
                payload,
                existing: candidate.range,
            });
        }

        // Check whether the next range overlaps with the new range.
        if let Some((start, candidate)) = cursor.peek_next()
            && *start < range.end
        {
            return Err(TryInsertError::Overlap {
                payload,
                existing: candidate.range,
            });
        }

        // No overlap found, insert the new range.
        let free_range = self.find_free_range_at(range.start).unwrap_or_else(|| {
            unreachable_inconsistency!();
        });
        debug_assert!(free_range.contains_range(range));
        self.remove_free_range(free_range);

        self.map.insert(range.start, RangeInSet { range, payload });

        if free_range.start < range.start {
            let new_free_range = AddrRange::new(free_range.start, range.start);
            self.insert_free_range(new_free_range);
        }

        if free_range.end > range.end {
            let new_free_range = AddrRange::new(range.end, free_range.end);
            self.insert_free_range(new_free_range);
        }

        Ok(())
    }

    /// Finds a free range of at least the given size.
    ///
    /// Returns the smallest free range whose size is at least `size`, or `None` when no free range
    /// is large enough. Among free ranges of the same size, the one with the lowest start address
    /// is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), ())?;
    /// assert_eq!(set.find_free_range(30), Some(va_range!(40..100)));
    /// assert_eq!(set.find_free_range(70), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn find_free_range(&self, size: usize) -> Option<AddrRange<M>> {
        let cursor = self
            .free_ranges_by_size
            .lower_bound(Bound::Included(&AddrRangeBySize::dummy_lower(size)));
        Some(cursor.peek_next()?.0)
    }

    /// Finds the free range containing the given address.
    ///
    /// Returns the free range containing the address, or `None` when the address is occupied or
    /// outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..40), ())?;
    /// assert_eq!(set.find_free_range_at(va!(10)), Some(va_range!(0..20)));
    /// assert_eq!(set.find_free_range_at(va!(30)), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn find_free_range_at(&self, addr: M) -> Option<AddrRange<M>> {
        let cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));

        if let Some(candidate) = cursor.peek_next()
            && candidate.start == addr
        {
            return Some(candidate.0);
        }

        let candidate = cursor.peek_prev()?;
        candidate.contains(addr).then_some(candidate.0)
    }

    /// Finds the free range ending at the given address.
    ///
    /// Returns `None` when the previous free range does not end exactly at the address.
    fn find_free_range_ending_at(&self, addr: M) -> Option<AddrRange<M>> {
        let cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));
        let candidate = cursor.peek_prev()?;

        (candidate.end == addr).then_some(candidate.0)
    }

    /// Finds the free range starting at the given address.
    ///
    /// Returns `None` when the next free range does not start exactly at the address.
    fn find_free_range_starting_at(&self, addr: M) -> Option<AddrRange<M>> {
        let cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));
        let candidate = cursor.peek_next()?;

        (candidate.start == addr).then_some(candidate.0)
    }

    /// Gets the address limit of the range set.
    ///
    /// Returns the fixed address range that bounds all occupied and free ranges tracked by the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let set = RangeSet::<VirtAddr>::new(va_range!(0..100));
    /// assert_eq!(set.limit(), va_range!(0..100));
    /// ```
    pub const fn limit(&self) -> AddrRange<M> {
        self.limit
    }

    /// Returns an iterator over the occupied ranges.
    ///
    /// The ranges are yielded in increasing start-address order, each with a reference to its
    /// payload.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(60..80), "stack")?;
    /// set.try_insert(va_range!(20..40), "kernel")?;
    /// let payloads: Vec<&str> = set.iter().map(|(_, payload)| *payload).collect();
    /// assert_eq!(payloads, ["kernel", "stack"]);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (AddrRange<M>, &P)> {
        self.map.values().map(|entry| (entry.range, &entry.payload))
    }

    /// Returns an iterator over the free ranges by address.
    ///
    /// The ranges are yielded in increasing start-address order.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..90), ())?;
    /// let free: Vec<AddrRange<VirtAddr>> = set.free_ranges_by_addr().collect();
    /// assert_eq!(
    ///     free,
    ///     [va_range!(0..20), va_range!(90..100)]
    /// );
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn free_ranges_by_addr(&self) -> impl Iterator<Item = AddrRange<M>> {
        self.free_ranges_by_addr.iter().map(|range| range.0)
    }

    /// Returns an iterator over the free ranges by size.
    ///
    /// The ranges are yielded in increasing size order, and ranges of the same size are yielded in
    /// increasing start-address order.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr, va_range};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(va_range!(0..100));
    /// set.try_insert(va_range!(20..90), ())?;
    /// let free: Vec<AddrRange<VirtAddr>> = set.free_ranges_by_size().collect();
    /// assert_eq!(
    ///     free,
    ///     [va_range!(90..100), va_range!(0..20)]
    /// );
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn free_ranges_by_size(&self) -> impl Iterator<Item = AddrRange<M>> {
        self.free_ranges_by_size.iter().map(|range| range.0)
    }
}

/// A debug view of the occupied ranges in a range set.
///
/// It formats the ranges as a map from each range to its payload.
struct OccupiedRangesDebug<'a, M: MemoryAddr, P, A: Allocator + Clone>(&'a RangeSet<M, P, A>);

/// Formats occupied ranges for debugging.
///
/// The ranges are written in increasing start-address order.
impl<M: MemoryAddr + fmt::Debug, P: fmt::Debug, A: Allocator + Clone> fmt::Debug
    for OccupiedRangesDebug<'_, M, P, A>
{
    /// Writes the occupied ranges as a map.
    ///
    /// Each entry maps a range to its payload.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

/// A debug view of the free ranges in a range set.
///
/// It formats the ranges as a list.
struct FreeRangesDebug<'a, M: MemoryAddr, P, A: Allocator + Clone>(&'a RangeSet<M, P, A>);

/// Formats free ranges for debugging.
///
/// The ranges are written in increasing start-address order.
impl<M: MemoryAddr + fmt::Debug, P, A: Allocator + Clone> fmt::Debug
    for FreeRangesDebug<'_, M, P, A>
{
    /// Writes the free ranges as a list.
    ///
    /// The size-ordered index contains the same ranges and is not written.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.free_ranges_by_addr())
            .finish()
    }
}

/// Formats range sets for debugging.
///
/// The output lists the limit, the occupied ranges with their payloads, and the free ranges by
/// address.
impl<M: MemoryAddr + fmt::Debug, P: fmt::Debug, A: Allocator + Clone> fmt::Debug
    for RangeSet<M, P, A>
{
    /// Writes the limit, occupied ranges, and free ranges.
    ///
    /// The allocator is not written.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RangeSet")
            .field("limit", &self.limit)
            .field("occupied", &OccupiedRangesDebug(self))
            .field("free", &FreeRangesDebug(self))
            .finish()
    }
}
