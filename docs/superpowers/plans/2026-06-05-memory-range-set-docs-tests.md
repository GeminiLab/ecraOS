# Memory Range Set Documentation and Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete documentation and tests for `memory_range_set`, while fixing the confirmed free-range lookup, removal, and out-of-limit insertion behavior.

**Architecture:** Keep the crate centered on `RangeSet`, with occupied ranges stored by start address and free ranges indexed both by address and by size. Add a small public error enum for insertion failures so overlap and limit violations are explicit. Put unit tests in `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`, leaving production code in `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`.

**Tech Stack:** Rust 2024, `#![no_std]`, `alloc`, `memory_addr::AddrRange`, `memory_addr::MemoryAddr`, nightly features already used by the crate.

---

## File Structure

- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`
  - Add crate-level and item-level documentation.
  - Add `#[cfg(test)] mod test;`.
  - Add an insertion error type.
  - Fix `find_free_range_at` to find the predecessor free range.
  - Fix `remove` to return removed ranges to the free indexes and coalesce adjacent free ranges.
  - Update `try_insert` to reject out-of-limit ranges using the new error type.
- Create: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`
  - Add focused unit tests for construction, insertion, lookup, mutation, removal, free-range queries, overlap errors, and limit errors.

Do not restructure the crate or revive the disabled `#[cfg(false)] mod arena`; it is out of scope.

---

### Task 1: Add Tests for Confirmed Current Behavior and Bugs

**Files:**
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`
- Create: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`

- [ ] **Step 1: Add test module declaration**

In `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`, near the other module declarations, add:

```rust
#[cfg(test)]
mod test;
```

- [ ] **Step 2: Create unit test file with address helpers**

Create `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`:

```rust
use memory_addr::{AddrRange, VirtAddr};

use crate::RangeSet;

fn va(addr: usize) -> VirtAddr {
    addr.into()
}

fn range(start: usize, end: usize) -> AddrRange<VirtAddr> {
    AddrRange::new(va(start), va(end))
}
```

Use `VirtAddr` from `memory_addr` unless it is unavailable; if it is unavailable, inspect the crate's exported address types and use the simplest concrete `MemoryAddr` type provided by `memory_addr`.

- [ ] **Step 3: Write failing tests for initialization and free lookup inside a range**

Add tests:

```rust
#[test]
fn new_set_starts_with_entire_limit_free() {
    let set = RangeSet::<VirtAddr>::new(range(0, 100));

    assert_eq!(set.limit(), range(0, 100));
    assert!(set.find(va(0)).is_none());
    assert_eq!(set.find_free_range(100), Some(range(0, 100)));
    assert_eq!(set.find_free_range(101), None);
    assert_eq!(set.find_free_range_at(va(0)), Some(range(0, 100)));
    assert_eq!(set.find_free_range_at(va(50)), Some(range(0, 100)));
    assert_eq!(set.find_free_range_at(va(100)), None);
}
```

Expected: `find_free_range_at(va(50))` fails before the implementation fix.

- [ ] **Step 4: Write failing test for insert splitting free range**

Add:

```rust
#[test]
fn insert_middle_range_splits_free_range() {
    let mut set = RangeSet::new(range(0, 100));

    assert_eq!(set.try_insert(range(20, 40), "kernel"), Ok(()));

    assert_eq!(set.find(va(20)), Some((range(20, 40), &"kernel")));
    assert_eq!(set.find(va(39)), Some((range(20, 40), &"kernel")));
    assert!(set.find(va(40)).is_none());
    assert_eq!(set.find_free_range_at(va(10)), Some(range(0, 20)));
    assert_eq!(set.find_free_range_at(va(20)), None);
    assert_eq!(set.find_free_range_at(va(50)), Some(range(40, 100)));
}
```

Expected: this fails before fixing `find_free_range_at`, likely by panic in `try_insert`.

- [ ] **Step 5: Write failing test for remove freeing and coalescing ranges**

Add:

```rust
#[test]
fn remove_returns_range_to_free_set_and_coalesces_neighbors() {
    let mut set = RangeSet::new(range(0, 100));
    assert_eq!(set.try_insert(range(20, 40), "a"), Ok(()));
    assert_eq!(set.try_insert(range(60, 80), "b"), Ok(()));

    assert_eq!(set.remove(va(25)), Some((range(20, 40), "a")));
    assert!(set.find(va(25)).is_none());
    assert_eq!(set.find_free_range_at(va(20)), Some(range(0, 60)));

    assert_eq!(set.remove(va(70)), Some((range(60, 80), "b")));
    assert_eq!(set.find_free_range_at(va(50)), Some(range(0, 100)));
    assert_eq!(set.find_free_range(100), Some(range(0, 100)));
}
```

Expected: this fails before fixing `remove`.

- [ ] **Step 6: Run the targeted tests and confirm failures**

Run:

```bash
cargo test -p memory_range_set --lib
```

Expected: at least the tests for free lookup, insertion, and removal fail before implementation fixes.

---

### Task 2: Introduce Explicit Insertion Errors

**Files:**
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`

- [ ] **Step 1: Add tests for overlap and out-of-limit errors**

Add to `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`:

```rust
use crate::TryInsertError;

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
```

Expected: these tests do not compile until the error type and signature change are implemented.

- [ ] **Step 2: Add `TryInsertError`**

In `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`, before `RangeSet`, add:

```rust
/// Errors returned when inserting a range into a [`RangeSet`].
///
/// The error carries the payload back to the caller so failed insertions do not
/// drop caller-owned state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryInsertError<M: MemoryAddr, P> {
    /// The requested range overlaps an existing occupied range.
    ///
    /// The `existing` range is the occupied range that prevented insertion.
    Overlap {
        /// The payload originally passed to [`RangeSet::try_insert`].
        payload: P,
        /// The occupied range that overlaps the requested range.
        existing: AddrRange<M>,
    },
    /// The requested range is not fully contained in the set limit.
    ///
    /// A range set only manages addresses within its limit.
    OutOfLimit {
        /// The payload originally passed to [`RangeSet::try_insert`].
        payload: P,
        /// The requested range that was outside the limit.
        range: AddrRange<M>,
        /// The limit of the range set.
        limit: AddrRange<M>,
    },
}
```

Keep documentation wording aligned with project doc-comment rules.

- [ ] **Step 3: Change `try_insert` signature and error construction**

Change:

```rust
pub fn try_insert(&mut self, range: AddrRange<A>, payload: P) -> Result<(), (P, AddrRange<A>)>
```

to:

```rust
pub fn try_insert(
    &mut self,
    range: AddrRange<A>,
    payload: P,
) -> Result<(), TryInsertError<A, P>>
```

At the start of the function, add:

```rust
if !self.limit.contains_range(range) {
    return Err(TryInsertError::OutOfLimit {
        payload,
        range,
        limit: self.limit,
    });
}
```

Replace overlap returns with:

```rust
return Err(TryInsertError::Overlap {
    payload,
    existing: candidate.range,
});
```

- [ ] **Step 4: Run tests and confirm only implementation-related failures remain**

Run:

```bash
cargo test -p memory_range_set --lib
```

Expected: tests compile. Free lookup and remove behavior may still fail until later tasks are complete.

---

### Task 3: Fix Free-Range Lookup and Removal Coalescing

**Files:**
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`

- [ ] **Step 1: Add broader tests for adjacent insertions and free range ordering**

Add:

```rust
#[test]
fn adjacent_ranges_do_not_overlap() {
    let mut set = RangeSet::new(range(0, 100));

    assert_eq!(set.try_insert(range(20, 40), 1), Ok(()));
    assert_eq!(set.try_insert(range(40, 60), 2), Ok(()));
    assert_eq!(set.try_insert(range(0, 20), 3), Ok(()));

    assert_eq!(set.find(va(0)), Some((range(0, 20), &3)));
    assert_eq!(set.find(va(20)), Some((range(20, 40), &1)));
    assert_eq!(set.find(va(40)), Some((range(40, 60), &2)));
    assert_eq!(set.find_free_range_at(va(60)), Some(range(60, 100)));
}

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
```

- [ ] **Step 2: Fix `find_free_range_at`**

Replace current implementation with deterministic exact-start plus predecessor lookup. First check whether a free range starts exactly at `addr`; if not, check the previous free range:

```rust
pub fn find_free_range_at(&self, addr: A) -> Option<AddrRange<A>> {
    let mut cursor = self
        .free_ranges_by_addr
        .lower_bound(Bound::Included(&AddrRangeByStart::dummy_lower(addr)));

    if let Some(candidate) = cursor.next() {
        if candidate.start == addr {
            return Some(candidate.0);
        }
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
```

Do not implement this as only `cursor.next()` or only `peek_prev()`: the required behavior is to find a free range starting exactly at `addr` when present, otherwise find the free range whose start is less than `addr` and check whether the half-open range contains `addr`.

- [ ] **Step 3: Add a private helper for removing a free range from both indexes**

Inside `impl<A: MemoryAddr, P> RangeSet<A, P>`, add:

```rust
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
```

Add a concise two-paragraph doc comment for this private helper, because the project requires doc comments on all items.

- [ ] **Step 4: Add a private helper for inserting a free range into both indexes**

Add:

```rust
fn insert_free_range(&mut self, range: AddrRange<A>) {
    self.free_ranges_by_addr
        .insert(AddrRangeByStart::new(range));
    self.free_ranges_by_size
        .insert(AddrRangeBySize::new(range));
}
```

Add a concise two-paragraph doc comment for this private helper, because the project requires doc comments on all items.

Then use these helpers in `new`, `try_insert`, and `remove` where appropriate. Do not over-abstract beyond these two repeated operations.

- [ ] **Step 5: Fix `remove` to coalesce free ranges**

After removing the occupied value from `map`, compute a merged free range:

```rust
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
```

Implement the two small lookup helpers. Add concise two-paragraph doc comments for both helpers, because the project requires doc comments on all items:

```rust
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
```

If `BTreeSet` cursor borrowing makes removal awkward, copy `AddrRange` values before mutating the sets. `AddrRange` is copy in existing code usage.

- [ ] **Step 6: Run unit tests**

Run:

```bash
cargo test -p memory_range_set --lib
```

Expected: all unit tests pass.

---

### Task 4: Complete Documentation and Doctest Examples

**Files:**
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`

- [ ] **Step 1: Add crate-level documentation**

At the top of `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`, before attributes, add:

```rust
//! Non-overlapping memory range sets.
//!
//! This crate provides [`RangeSet`], a small data structure for tracking occupied
//! address ranges inside a fixed limit. Each occupied range carries a caller-defined
//! payload, while the set also keeps indexes of the remaining free ranges.
```

Ensure wording follows the project's doc-comment style.

- [ ] **Step 2: Add documentation for internal range wrappers**

Add doc comments to `RangeInSet`, `AddrRangeBySize`, and `AddrRangeByStart`. Keep summaries concise and include a blank line before details.

Example for `AddrRangeByStart`:

```rust
/// An address range ordered by its start address.
///
/// This wrapper lets the free-range address index search for ranges by their
/// position in the managed address space.
```

- [ ] **Step 3: Add documentation for `RangeSet`**

Add:

```rust
/// A set of non-overlapping occupied memory ranges.
///
/// The set manages a fixed address limit. Inserted ranges are treated as occupied
/// and are associated with a payload, while the remaining portions of the limit are
/// tracked as free ranges.
```

Include examples in method docs instead of on the type if that keeps doctests smaller.

- [ ] **Step 4: Add public method documentation with examples**

Add or revise docs for:

- `RangeSet::new`
- `RangeSet::find`
- `RangeSet::find_mut`
- `RangeSet::remove`
- `RangeSet::try_insert`
- `RangeSet::find_free_range`
- `RangeSet::find_free_range_at`
- `RangeSet::limit`
- `TryInsertError`

Use compact examples like:

```rust
/// # Examples
///
/// ```
/// use memory_addr::{AddrRange, VirtAddr};
/// use memory_range_set::RangeSet;
///
/// let mut set = RangeSet::new(AddrRange::new(VirtAddr::from(0), VirtAddr::from(100)));
/// set.try_insert(AddrRange::new(VirtAddr::from(20), VirtAddr::from(40)), "kernel")?;
/// assert_eq!(set.find(VirtAddr::from(30)).map(|(_, payload)| *payload), Some("kernel"));
/// # Ok::<(), memory_range_set::TryInsertError<VirtAddr, &str>>(())
/// ```
```

If `VirtAddr::from(usize)` is not accepted, use the conversion syntax verified by unit tests.

- [ ] **Step 5: Avoid examples for awkward private/internal items**

Do not add examples for disabled `arena`, private ordering wrappers, or tiny private helpers unless the example would clarify public behavior. The user explicitly said examples are not required for every item.

- [ ] **Step 6: Run doctests**

Run:

```bash
cargo test -p memory_range_set --doc
```

Expected: all doctests pass.

---

### Task 5: Final Verification and Formatting

**Files:**
- Modify: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/lib.rs`
- Create: `/home/aarkegz/source/ecra/ecraOS/memory_range_set/src/test.rs`

- [ ] **Step 1: Run rustfmt**

Run:

```bash
cargo fmt --package memory_range_set
```

Expected: command succeeds and formats `memory_range_set` files.

- [ ] **Step 2: Run unit tests and doctests together**

Run:

```bash
cargo test -p memory_range_set
```

Expected: all unit tests and doctests pass.

- [ ] **Step 3: Run workspace clippy target check if feasible**

Run:

```bash
cargo clippy --workspace --target x86_64-unknown-none
```

Expected: no clippy errors. If this fails for pre-existing unrelated workspace issues, record the exact failure and also run a narrower check for `memory_range_set` if possible.

- [ ] **Step 4: Inspect final diff**

Run:

```bash
git diff -- memory_range_set/src/lib.rs memory_range_set/src/test.rs
```

Expected: diff contains only documentation, tests, `TryInsertError`, and the confirmed bug fixes. No unrelated refactors.

- [ ] **Step 5: Report verification evidence**

Summarize:

- Which tests were added.
- Which bugs were fixed.
- Exact verification commands and whether they passed.
- Any unresolved failures, with exact error messages if present.

Do not claim completion until verification output has been observed.
