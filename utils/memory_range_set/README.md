# `memory_range_set`

`memory_range_set` provides [`RangeSet`], a set of non-overlapping occupied address ranges inside a
fixed limit. Each occupied range carries a caller-defined payload, and the remaining parts of the
limit are indexed as free ranges by address and by size. Addresses use the `memory_addr` address
types, such as `VirtAddr` and `PhysAddr`.

The crate is `no_std` and requires `alloc`. It needs a nightly toolchain because it uses the
`allocator_api`, `btree_cursors`, and `btreemap_alloc` features.

## Usage

Add the crate as a dependency:

```toml
[dependencies]
memory_range_set = "0.1"
```

Then create a set with its limit, insert occupied ranges, and query the occupied and free ranges:

```rust
use memory_addr::{va, va_range};
use memory_range_set::RangeSet;

let mut set = RangeSet::new(va_range!(0x0000..0x8000));
assert!(set.try_insert(va_range!(0x1000..0x3000), "kernel").is_ok());
assert!(set.try_insert(va_range!(0x5000..0x6000), "stack").is_ok());

assert_eq!(
    set.find(va!(0x2000)),
    Some((va_range!(0x1000..0x3000), &"kernel"))
);
assert_eq!(
    set.find_free_range(0x1800),
    Some(va_range!(0x3000..0x5000))
);

assert_eq!(
    set.remove(va!(0x5800)),
    Some((va_range!(0x5000..0x6000), "stack"))
);
assert_eq!(
    set.find_free_range_at(va!(0x5800)),
    Some(va_range!(0x3000..0x8000))
);
```

[`RangeSet::new`] allocates the internal B-tree nodes with the global allocator, and
[`RangeSet::new_in`] uses a given allocator instead.

## Occupied ranges

All ranges are half-open, so ranges that only share an endpoint do not overlap.

- [`RangeSet::try_insert`] adds an occupied range with its payload. It fails with
  [`TryInsertError::Empty`] when the range is empty, with [`TryInsertError::OutOfLimit`] when the
  range is not fully inside the limit, and with [`TryInsertError::Overlap`] when the range overlaps
  an occupied range. Every error returns the payload to the caller.
- [`RangeSet::find`] and [`RangeSet::find_mut`] return the occupied range that contains an
  address, together with a reference to its payload.
- [`RangeSet::remove`] removes the occupied range that contains an address and returns the range
  and its payload.
- [`RangeSet::iter`] iterates over the occupied ranges and their payloads by start address.

## Free ranges

A new set has one free range that covers the whole limit, or no free range when the limit is empty.
Inserting a range splits the free range that contains it, and removing a range merges the freed
space with adjacent free ranges. Free ranges are therefore never empty and never adjacent to each
other.

- [`RangeSet::find_free_range`] returns the smallest free range whose size is at least the
  requested size. Among free ranges of the same size, it returns the one with the lowest start
  address.
- [`RangeSet::find_free_range_at`] returns the free range that contains an address.
- [`RangeSet::free_ranges_by_addr`] iterates over free ranges by start address, and
  [`RangeSet::free_ranges_by_size`] iterates over them by size and then by start address.
