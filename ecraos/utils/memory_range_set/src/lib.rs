//! Non-overlapping memory range sets.
//!
//! This crate provides [`RangeSet`], a small data structure for tracking occupied address ranges
//! inside a fixed limit. Each occupied range carries a caller-defined payload, while the set also
//! keeps indexes of the remaining free ranges.

#![no_std]
#![feature(allocator_api)]
#![feature(btree_cursors)]

extern crate alloc;

use alloc::{
    alloc::{Allocator, Global},
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
};
use core::{
    cmp::Ordering,
    ops::{Bound, Deref, DerefMut},
};

use memory_addr::{AddrRange, AddrRangeBounds, MemoryAddr};

/// Tests for range set behavior.
///
/// Defines behavior specifications for range insertion, removal, lookup, and free-range queries.
#[cfg(test)]
mod test;

#[cfg(false)]
mod arena {
    use core::{mem::ManuallyDrop, ptr::NonNull};

    use alloc::boxed::Box;
    use nonmax::NonMaxU32;

    /// The number of chunks in the arena.
    const CHUNKS_COUNT: usize = 16;
    /// The size of the first chunk in the arena.
    const FIRST_CHUNK_SIZE: usize = 16;

    const fn chunk_size(chunk: usize) -> Option<usize> {
        if chunk >= CHUNKS_COUNT {
            None
        } else {
            Some(FIRST_CHUNK_SIZE << chunk)
        }
    }

    #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[repr(transparent)]
    struct ArenaIndex(NonMaxU32);

    impl ArenaIndex {
        const OFFSET_BITS: usize = 24;

        const unsafe fn chunk(self) -> usize {
            self.0.get() as usize >> Self::OFFSET_BITS
        }

        const unsafe fn offset(self) -> usize {
            self.0.get() as usize & ((1 << Self::OFFSET_BITS) - 1)
        }

        const fn chunk_offset(self) -> Option<(usize, usize)> {
            let chunk = unsafe { self.chunk() };
            let Some(chunk_size) = chunk_size(chunk) else {
                return None;
            };
            let offset = unsafe { self.offset() };
            if offset >= chunk_size {
                None
            } else {
                Some((chunk, offset))
            }
        }

        const unsafe fn new_unchecked(chunk: usize, offset: usize) -> Self {
            Self(NonMaxU32::new(((chunk as u32) << Self::OFFSET_BITS) | offset as u32).unwrap())
        }
    }

    const _: () =
        assert!(core::mem::size_of::<Option<ArenaIndex>>() == core::mem::size_of::<NonMaxU32>());

    union ArenaSlot<T> {
        free_next: Option<ArenaIndex>,
        data: ManuallyDrop<T>,
    }

    struct RangeArena<T> {
        chunks: [Option<NonNull<ArenaSlot<T>>>; CHUNKS_COUNT],
        occupied: [Option<NonNull<u8>>; CHUNKS_COUNT],
        free_list_head: Option<ArenaIndex>,
        chunks_allocated: u32,
    }

    impl<T> RangeArena<T> {
        const fn new() -> Self {
            Self {
                chunks: [None; CHUNKS_COUNT],
                occupied: [None; CHUNKS_COUNT],
                free_list_head: None,
                chunks_allocated: 0,
            }
        }

        const fn slot(&self, index: ArenaIndex) -> Option<&ArenaSlot<T>> {
            let Some((chunk, offset)) = index.chunk_offset() else {
                return None;
            };

            match self.chunks[chunk] {
                Some(chunk) => {
                    // SAFETY: `index.chunk_offset()` has already checked that the offset is within the
                    // chunk.
                    unsafe { Some(chunk.as_ptr().add(offset).as_ref_unchecked()) }
                }
                None => None,
            }
        }

        const fn slot_mut(&mut self, index: ArenaIndex) -> Option<&mut ArenaSlot<T>> {
            let Some((chunk, offset)) = index.chunk_offset() else {
                return None;
            };
            match self.chunks[chunk] {
                Some(chunk) => {
                    // SAFETY: `index.chunk_offset()` has already checked that the offset is within the
                    // chunk.
                    unsafe { Some(chunk.as_ptr().add(offset).as_mut_unchecked()) }
                }
                None => None,
            }
        }

        const fn is_occupied(&self, index: ArenaIndex) -> Option<bool> {
            let Some((chunk, offset)) = index.chunk_offset() else {
                return None;
            };
            let Some(occupied_arr) = self.occupied[chunk] else {
                return None;
            };
            let byte_offset = offset / u8::BITS as usize;
            let byte_mask = 1 << (offset % u8::BITS as usize);
            // SAFETY: We know that the offset is within the chunk.
            Some(unsafe { *occupied_arr.as_ptr().add(byte_offset) & byte_mask != 0 })
        }

        const fn set_occupied(&mut self, index: ArenaIndex, occupied: bool) {
            let Some((chunk, offset)) = index.chunk_offset() else {
                return;
            };
            let Some(occupied_arr) = self.occupied[chunk] else {
                return;
            };

            let byte_offset = offset / u8::BITS as usize;
            let byte_mask = 1 << (offset % u8::BITS as usize);

            // SAFETY: We know that the offset is within the chunk.
            unsafe {
                let occupied_byte = occupied_arr.as_ptr().add(byte_offset);

                if occupied {
                    *occupied_byte |= byte_mask;
                } else {
                    *occupied_byte &= !byte_mask;
                }
            }
        }

        fn alloc(&mut self) -> Option<ArenaIndex> {
            match self.free_list_head {
                Some(index) => {
                    self.free_list_head = unsafe { self.slot(index).unwrap_unchecked().free_next };
                    self.set_occupied(index, true);
                    Some(index)
                }
                None => {
                    let chunk_index = self.chunks_allocated as _;
                    let chunk_size = chunk_size(chunk_index)?;
                    self.chunks_allocated += 1;

                    let mut chunk = Box::<[ArenaSlot<T>]>::new_uninit_slice(chunk_size);
                    for index in 0..(chunk_size - 1) {
                        // SAFETY: We know that the offset is within the chunk.
                        chunk[index].write(ArenaSlot {
                            free_next: Some(unsafe {
                                ArenaIndex::new_unchecked(chunk_index, (index + 1) as _)
                            }),
                        });
                    }
                    chunk[chunk_size - 1].write(ArenaSlot { free_next: None });

                    // SAFETY: We have just initialized the chunk.
                    let chunk = unsafe { chunk.assume_init() };
                    // SAFETY: This bitfield can be zeroed safely.
                    let occupied = unsafe {
                        Box::<[u8]>::new_zeroed_slice(chunk_size / u8::BITS as usize).assume_init()
                    };

                    self.chunks[chunk_index] =
                        Some(unsafe { NonNull::new_unchecked(Box::into_raw(chunk) as *mut _) });
                    self.occupied[chunk_index] =
                        Some(unsafe { NonNull::new_unchecked(Box::into_raw(occupied) as *mut _) });

                    // SAFETY: We know that the offset is within the chunk.
                    self.free_list_head =
                        Some(unsafe { ArenaIndex::new_unchecked(chunk_index, 0) });
                    self.alloc()
                }
            }
        }

        fn free(&mut self, index: ArenaIndex) -> Option<T> {
            if !self.is_occupied(index)? {
                return None;
            }

            self.set_occupied(index, false);
            let free_list_head = self.free_list_head;
            let slot = self.slot_mut(index)?;
            unsafe {
                let data = ManuallyDrop::take(&mut slot.data);
                slot.free_next = free_list_head;
                self.free_list_head = Some(index);
                Some(data)
            }
        }
    }
}

/// An occupied range stored in a range set.
///
/// This entry keeps the occupied address range together with the payload associated with that
/// range.
struct RangeInSet<M: MemoryAddr, P = ()> {
    range: AddrRange<M>,
    payload: P,
}

/// An address range ordered by its size.
///
/// This wrapper lets the free-range size index search for the smallest range that satisfies a
/// requested allocation size.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(transparent)]
struct AddrRangeBySize<M: MemoryAddr>(pub AddrRange<M>);

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

impl<M: MemoryAddr> Deref for AddrRangeBySize<M> {
    type Target = AddrRange<M>;

    /// Dereferences to the wrapped address range.
    ///
    /// Allows size-ordered range wrappers to be used where immutable address range access is needed.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<M: MemoryAddr> DerefMut for AddrRangeBySize<M> {
    /// Mutably dereferences to the wrapped address range.
    ///
    /// Allows size-ordered range wrappers to be used where mutable address range access is needed.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<M: MemoryAddr> PartialOrd for AddrRangeBySize<M> {
    /// Partially compares ranges by size, then start address.
    ///
    /// Provides the same ordering keys as [`Ord`] while preserving the trait contract.
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

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

impl<M: MemoryAddr> Deref for AddrRangeByStart<M> {
    type Target = AddrRange<M>;

    /// Dereferences to the wrapped address range.
    ///
    /// Allows start-ordered range wrappers to be used where immutable address range access is needed.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<M: MemoryAddr> DerefMut for AddrRangeByStart<M> {
    /// Mutably dereferences to the wrapped address range.
    ///
    /// Allows start-ordered range wrappers to be used where mutable address range access is needed.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<M: MemoryAddr> PartialOrd for AddrRangeByStart<M> {
    /// Partially compares ranges by start address, then end address.
    ///
    /// Provides the same ordering keys as [`Ord`] while preserving the trait contract.
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

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
/// use memory_addr::{AddrRange, VirtAddr};
/// use memory_range_set::{RangeSet, TryInsertError};
///
/// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
/// set.try_insert(AddrRange::new(20.into(), 40.into()), "kernel")?;
///
/// assert_eq!(
///     set.try_insert(AddrRange::new(30.into(), 50.into()), "driver"),
///     Err(TryInsertError::Overlap {
///         payload: "driver",
///         existing: AddrRange::new(20.into(), 40.into()),
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
}

/// A set of non-overlapping occupied memory ranges.
///
/// The set manages a fixed address limit. Inserted ranges are treated as occupied and are
/// associated with a payload, while the remaining portions of the limit are tracked as free ranges.
pub struct RangeSet<M: MemoryAddr, P = (), A: Allocator = Global>
where
    A: Clone,
{
    limit: AddrRange<M>,
    map: BTreeMap<M, RangeInSet<M, P>, A>,
    free_ranges_by_size: BTreeSet<AddrRangeBySize<M>, A>,
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

impl<A: MemoryAddr, P> RangeSet<A, P> {
    /// Creates a range set with the given address limit.
    ///
    /// The new set starts with no occupied ranges and with the whole limit available as a free
    /// range.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let set = RangeSet::<VirtAddr>::new(AddrRange::new(0.into(), 100.into()));
    /// assert_eq!(set.limit(), AddrRange::new(0.into(), 100.into()));
    /// assert_eq!(set.find_free_range(100), Some(AddrRange::new(0.into(), 100.into())));
    /// ```
    pub fn new(limit: AddrRange<A>) -> Self {
        let mut result = Self {
            limit,
            map: BTreeMap::new(),
            free_ranges_by_size: BTreeSet::new(),
            free_ranges_by_addr: BTreeSet::new(),
        };

        result.insert_free_range(limit);

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
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), "kernel")?;
    /// assert_eq!(set.find(30.into()).map(|(_, payload)| *payload), Some("kernel"));
    /// assert_eq!(set.find(50.into()), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn find(&self, addr: A) -> Option<(AddrRange<A>, &P)> {
        let cursor = self.map.lower_bound(Bound::Excluded(&addr));
        let (start, candidate) = cursor.peek_prev()?;

        if candidate.range.contains(addr) {
            let value = self.map.get(start)?;
            Some((value.range, &value.payload))
        } else {
            None
        }
    }

    /// Finds the range containing the given address mutably.
    ///
    /// Returns the occupied range and a mutable reference to its payload, or `None` when the address
    /// is free or outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), 1)?;
    /// if let Some((_, payload)) = set.find_mut(30.into()) {
    ///     *payload = 2;
    /// }
    /// assert_eq!(set.find(30.into()).map(|(_, payload)| *payload), Some(2));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, i32>>(())
    /// ```
    pub fn find_mut(&mut self, addr: A) -> Option<(AddrRange<A>, &mut P)> {
        let cursor = self.map.lower_bound(Bound::Excluded(&addr));
        let (start, candidate) = cursor.peek_prev()?;

        if candidate.range.contains(addr) {
            let start = *start;
            let value = self.map.get_mut(&start)?;
            Some((value.range, &mut value.payload))
        } else {
            None
        }
    }

    /// Removes the range containing the given address.
    ///
    /// Returns the removed range and its payload, or `None` when the address is free or outside the
    /// set limit. The removed range becomes free and is coalesced with neighboring free ranges.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), "kernel")?;
    /// assert_eq!(
    ///     set.remove(30.into()),
    ///     Some((AddrRange::new(20.into(), 40.into()), "kernel"))
    /// );
    /// assert_eq!(set.find_free_range_at(30.into()), Some(AddrRange::new(0.into(), 100.into())));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn remove(&mut self, addr: A) -> Option<(AddrRange<A>, P)> {
        let cursor = self.map.lower_bound(Bound::Excluded(&addr));
        let (start, candidate) = cursor.peek_prev()?;

        if !candidate.range.contains(addr) {
            return None;
        }

        let start = *start;
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
    fn remove_free_range(&mut self, range: AddrRange<A>) {
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
    fn insert_free_range(&mut self, range: AddrRange<A>) {
        self.free_ranges_by_addr
            .insert(AddrRangeByStart::new(range));
        self.free_ranges_by_size.insert(AddrRangeBySize::new(range));
    }

    /// Tries to insert a range into the set.
    ///
    /// Inserts the range as occupied and associates it with the payload. Returns an error with the
    /// payload when the range overlaps an existing occupied range or falls outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), "kernel")?;
    /// assert_eq!(set.find(30.into()).map(|(_, payload)| *payload), Some("kernel"));
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
    /// ```
    pub fn try_insert(
        &mut self,
        range: AddrRange<A>,
        payload: P,
    ) -> Result<(), TryInsertError<A, P>> {
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
    /// Returns the smallest known free range that can satisfy the requested size, or `None` when no
    /// suitable free range exists.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), ())?;
    /// assert_eq!(set.find_free_range(30), Some(AddrRange::new(40.into(), 100.into())));
    /// assert_eq!(set.find_free_range(70), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn find_free_range(&self, size: usize) -> Option<AddrRange<A>> {
        let mut cursor = self
            .free_ranges_by_size
            .lower_bound(Bound::Included(&AddrRangeBySize::dummy_lower(size)));
        Some(cursor.next()?.0)
    }

    /// Finds the free range containing the given address.
    ///
    /// Returns the free range containing the address, or `None` when the address is occupied or
    /// outside the set limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let mut set = RangeSet::new(AddrRange::new(0.into(), 100.into()));
    /// set.try_insert(AddrRange::new(20.into(), 40.into()), ())?;
    /// assert_eq!(set.find_free_range_at(10.into()), Some(AddrRange::new(0.into(), 20.into())));
    /// assert_eq!(set.find_free_range_at(30.into()), None);
    /// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, ()>>(())
    /// ```
    pub fn find_free_range_at(&self, addr: A) -> Option<AddrRange<A>> {
        let mut cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));

        if let Some(candidate) = cursor.next()
            && candidate.start == addr
        {
            return Some(candidate.0);
        }

        let cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));
        let candidate = cursor.peek_prev()?;

        if candidate.contains(addr) {
            Some(candidate.0)
        } else {
            None
        }
    }

    /// Finds the free range ending at the given address.
    ///
    /// Returns `None` when the previous free range does not end exactly at the address.
    fn find_free_range_ending_at(&self, addr: A) -> Option<AddrRange<A>> {
        let cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));
        let candidate = cursor.peek_prev()?;

        if candidate.end == addr {
            Some(candidate.0)
        } else {
            None
        }
    }

    /// Finds the free range starting at the given address.
    ///
    /// Returns `None` when the next free range does not start exactly at the address.
    fn find_free_range_starting_at(&self, addr: A) -> Option<AddrRange<A>> {
        let mut cursor = self
            .free_ranges_by_addr
            .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));
        let candidate = cursor.next()?;

        if candidate.start == addr {
            Some(candidate.0)
        } else {
            None
        }
    }

    /// Gets the address limit of the range set.
    ///
    /// Returns the fixed address range that bounds all occupied and free ranges tracked by the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_addr::{AddrRange, VirtAddr};
    /// use memory_range_set::RangeSet;
    ///
    /// let set = RangeSet::<VirtAddr>::new(AddrRange::new(0.into(), 100.into()));
    /// assert_eq!(set.limit(), AddrRange::new(0.into(), 100.into()));
    /// ```
    pub const fn limit(&self) -> AddrRange<A> {
        self.limit
    }

    /// Returns an iterator over the free ranges by address.
    pub fn free_ranges_by_addr(&self) -> impl Iterator<Item = AddrRange<A>> {
        self.free_ranges_by_addr.iter().map(|range| range.0)
    }

    /// Returns an iterator over the free ranges by size.
    pub fn free_ranges_by_size(&self) -> impl Iterator<Item = AddrRange<A>> {
        self.free_ranges_by_size.iter().map(|range| range.0)
    }
}
