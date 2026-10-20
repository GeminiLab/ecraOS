use alloc::{
    alloc::{AllocError, Allocator, Global, Layout},
    format,
    string::ToString,
    vec::Vec,
};
use core::{cell::Cell, ptr::NonNull};

use memory_addr::{AddrRange, VirtAddr, va};

use crate::{RangeSet, TryInsertError};

/// An allocator that counts its allocations.
///
/// It forwards every request to [`Global`] and records each allocation in a shared counter.
#[derive(Clone, Copy)]
struct CountingAllocator<'a>(&'a Cell<usize>);

/// Forwards allocation requests to the global allocator.
///
/// Every successful or failed allocation request increments the counter.
unsafe impl Allocator for CountingAllocator<'_> {
    /// Allocates memory with the global allocator.
    ///
    /// The counter is incremented before the request is forwarded.
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.0.set(self.0.get() + 1);
        Global.allocate(layout)
    }

    /// Deallocates memory with the global allocator.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with `layout`.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: Every allocation of this allocator comes from `Global`.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

/// Creates a virtual address range from numeric bounds.
///
/// Provides a concise helper for constructing test ranges with virtual addresses.
fn range(start: usize, end: usize) -> AddrRange<VirtAddr> {
    AddrRange::new(va!(start), va!(end))
}

/// Verifies that a new range set starts entirely free.
///
/// Checks the initial limit, allocation lookup state, and free-range queries.
#[test]
fn new_set_starts_with_entire_limit_free() {
    let set = RangeSet::<VirtAddr>::new(range(0, 100));

    assert_eq!(set.limit(), range(0, 100));
    assert!(set.find(va!(0)).is_none());
    assert_eq!(set.find_free_range(100), Some(range(0, 100)));
    assert_eq!(set.find_free_range(101), None);
    assert_eq!(set.find_free_range_at(va!(0)), Some(range(0, 100)));
    assert_eq!(set.find_free_range_at(va!(50)), Some(range(0, 100)));
    assert_eq!(set.find_free_range_at(va!(100)), None);
}

/// Verifies that a set with an empty limit has no free range.
///
/// Free ranges are never empty, so the empty limit is not recorded as a free range.
#[test]
fn new_set_with_empty_limit_has_no_free_range() {
    let set = RangeSet::<VirtAddr>::new(range(50, 50));

    assert_eq!(set.limit(), range(50, 50));
    assert_eq!(set.free_ranges_by_addr().count(), 0);
    assert_eq!(set.find_free_range(0), None);
}

/// Verifies that inserting a middle range splits the free range.
///
/// Checks allocated lookups and free-range queries around the inserted range.
#[test]
fn insert_middle_range_splits_free_range() {
    let mut set = RangeSet::new(range(0, 100));

    assert_eq!(set.try_insert(range(20, 40), "kernel"), Ok(()));

    assert_eq!(set.find(va!(20)), Some((range(20, 40), &"kernel")));
    assert_eq!(set.find(va!(39)), Some((range(20, 40), &"kernel")));
    assert!(set.find(va!(40)).is_none());
    assert_eq!(set.find_free_range_at(va!(10)), Some(range(0, 20)));
    assert_eq!(set.find_free_range_at(va!(20)), None);
    assert_eq!(set.find_free_range_at(va!(50)), Some(range(40, 100)));
}

/// Verifies that adjacent inserted ranges remain distinct.
///
/// Checks that adjacent ranges do not overlap and leave the remaining tail free.
#[test]
fn adjacent_ranges_do_not_overlap() {
    let mut set = RangeSet::new(range(0, 100));

    assert_eq!(set.try_insert(range(20, 40), 1), Ok(()));
    assert_eq!(set.try_insert(range(40, 60), 2), Ok(()));
    assert_eq!(set.try_insert(range(0, 20), 3), Ok(()));

    assert_eq!(set.find(va!(0)), Some((range(0, 20), &3)));
    assert_eq!(set.find(va!(20)), Some((range(20, 40), &1)));
    assert_eq!(set.find(va!(40)), Some((range(40, 60), &2)));
    assert_eq!(set.find_free_range_at(va!(60)), Some(range(60, 100)));
}

/// Verifies that free-range lookup chooses the smallest suitable range.
///
/// Checks that size-ordered lookup returns the smallest free range that can satisfy each request.
#[test]
fn find_free_range_returns_smallest_suitable_range() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(10, 20), ()), Ok(()));
    assert_eq!(set.try_insert(range(40, 50), ()), Ok(()));
    assert_eq!(set.try_insert(range(80, 90), ()), Ok(()));

    assert_eq!(set.find_free_range(10), Some(range(0, 10)));
    assert_eq!(set.find_free_range(11), Some(range(20, 40)));
    assert_eq!(set.find_free_range(30), Some(range(50, 80)));
    assert_eq!(set.find_free_range(31), None);
}

/// Verifies that insertion reports overlapping ranges.
///
/// Checks that failed overlapping insertions return the payload and the existing range.
#[test]
fn try_insert_reports_overlap_and_returns_payload() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(20, 40), "first"), Ok(()));

    assert_eq!(
        set.try_insert(range(30, 50), "second"),
        Err(TryInsertError::Overlap {
            payload: "second",
            existing: range(20, 40),
        })
    );

    assert_eq!(
        set.try_insert(range(10, 30), "third"),
        Err(TryInsertError::Overlap {
            payload: "third",
            existing: range(20, 40),
        })
    );
}

/// Verifies that insertion reports ranges outside the limit.
///
/// Checks that failed out-of-limit insertions return the payload, range, and set limit.
#[test]
fn try_insert_reports_out_of_limit_ranges() {
    let mut set = RangeSet::new(range(10, 100));

    assert_eq!(
        set.try_insert(range(0, 20), "low"),
        Err(TryInsertError::OutOfLimit {
            payload: "low",
            range: range(0, 20),
            limit: range(10, 100),
        })
    );

    assert_eq!(
        set.try_insert(range(90, 110), "high"),
        Err(TryInsertError::OutOfLimit {
            payload: "high",
            range: range(90, 110),
            limit: range(10, 100),
        })
    );
}

/// Verifies that removing ranges returns them to the free set.
///
/// Checks removal results and coalescing behavior across neighboring free ranges.
#[test]
fn remove_returns_range_to_free_set_and_coalesces_neighbors() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(20, 40), "a"), Ok(()));
    assert_eq!(set.try_insert(range(60, 80), "b"), Ok(()));

    assert_eq!(set.remove(va!(25)), Some((range(20, 40), "a")));
    assert!(set.find(va!(25)).is_none());
    assert_eq!(set.find_free_range_at(va!(20)), Some(range(0, 60)));

    assert_eq!(set.remove(va!(70)), Some((range(60, 80), "b")));
    assert_eq!(set.find_free_range_at(va!(50)), Some(range(0, 100)));
    assert_eq!(set.find_free_range(100), Some(range(0, 100)));
}

/// Verifies that insertion rejects empty ranges.
///
/// An empty range inside a free range leaves the free ranges unchanged, and an empty range at the
/// end of the limit after an occupied range is rejected without an inconsistency panic.
#[test]
fn try_insert_rejects_empty_ranges() {
    let mut set = RangeSet::new(range(0, 100));

    assert_eq!(
        set.try_insert(range(50, 50), "inside"),
        Err(TryInsertError::Empty {
            payload: "inside",
            range: range(50, 50),
        })
    );
    assert_eq!(
        set.free_ranges_by_addr().collect::<Vec<_>>(),
        [range(0, 100)]
    );
    assert_eq!(set.try_insert(range(40, 60), "spanning"), Ok(()));

    assert_eq!(set.try_insert(range(90, 100), "tail"), Ok(()));
    assert_eq!(
        set.try_insert(range(100, 100), "end"),
        Err(TryInsertError::Empty {
            payload: "end",
            range: range(100, 100),
        })
    );
}

/// Verifies that a set created with `new_in` allocates through the given allocator.
///
/// Insertion, lookup, and removal behave as with the global allocator.
#[test]
fn new_in_uses_given_allocator() {
    let allocations = Cell::new(0);
    let mut set = RangeSet::new_in(range(0, 100), CountingAllocator(&allocations));
    let initial_allocations = allocations.get();
    assert!(initial_allocations > 0);

    assert_eq!(set.try_insert(range(20, 40), "kernel"), Ok(()));
    assert!(allocations.get() > initial_allocations);
    assert_eq!(set.find(va!(30)), Some((range(20, 40), &"kernel")));
    assert_eq!(set.remove(va!(30)), Some((range(20, 40), "kernel")));
    assert_eq!(set.find_free_range(100), Some(range(0, 100)));
}

/// Verifies that occupied ranges are iterated by start address.
///
/// Each item contains the range and a reference to its payload.
#[test]
fn iter_yields_occupied_ranges_by_address() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(60, 80), "b"), Ok(()));
    assert_eq!(set.try_insert(range(20, 40), "a"), Ok(()));

    assert_eq!(
        set.iter().collect::<Vec<_>>(),
        [(range(20, 40), &"a"), (range(60, 80), &"b")]
    );
}

/// Verifies the debug representation of a range set.
///
/// The output lists the limit, the occupied ranges with their payloads, and the free ranges.
#[test]
fn debug_lists_limit_occupied_and_free_ranges() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(20, 40), "kernel"), Ok(()));

    assert_eq!(
        format!("{set:?}"),
        concat!(
            "RangeSet { limit: VA:0x0..VA:0x64, ",
            "occupied: {VA:0x14..VA:0x28: \"kernel\"}, ",
            "free: [VA:0x0..VA:0x14, VA:0x28..VA:0x64] }",
        )
    );
}

/// Verifies the messages and error trait of insertion errors.
///
/// The messages describe the rejected or conflicting ranges without the payload.
#[test]
fn try_insert_errors_display_ranges() {
    /// Accepts only types that implement the standard error trait.
    ///
    /// The function has no runtime behavior.
    fn assert_error<E: core::error::Error>(_: &E) {}

    let overlap = TryInsertError::Overlap {
        payload: "driver",
        existing: range(20, 40),
    };
    let out_of_limit = TryInsertError::OutOfLimit {
        payload: "low",
        range: range(0, 20),
        limit: range(10, 100),
    };
    let empty = TryInsertError::Empty {
        payload: "empty",
        range: range(50, 50),
    };

    assert_error(&overlap);
    assert_eq!(
        overlap.to_string(),
        "range overlaps the existing range VA:0x14..VA:0x28"
    );
    assert_eq!(
        out_of_limit.to_string(),
        "range VA:0x0..VA:0x14 is outside the limit VA:0xa..VA:0x64"
    );
    assert_eq!(empty.to_string(), "range VA:0x32..VA:0x32 is empty");
}
