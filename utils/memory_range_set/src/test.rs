use memory_addr::{AddrRange, VirtAddr, va};

use crate::{RangeSet, TryInsertError};

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
