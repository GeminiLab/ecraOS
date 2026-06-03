# exbuddy Allocator Test Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace existing allocator tests outside `exbuddy::pfn` with a gray-box test suite that validates all public `BuddyAllocator` allocation and free APIs under tiny 16B and 32B page sizes.

**Architecture:** Add a dedicated `exbuddy/src/tests.rs` module compiled only under `#[cfg(test)]`, keeping `pfn.rs` tests intact. The test module owns page-aligned host memory, initializes `BuddyAllocator` with `virt_phys_offset = 0`, builds multiple sections, and compares allocator behavior against a shadow page model. Random pressure tests use fresh seeds by default, support explicit replay, and checkpoint full `is_allocated` scans rather than scanning after every operation.

**Tech Stack:** Rust 2024, host `cargo test`, `std` under `#[cfg(test)]`, `memory_addr`, public `exbuddy` APIs only, a small in-test PRNG instead of adding dependencies.

---

## File Structure

- Modify `exbuddy/src/lib.rs`
  - Add `#[cfg(test)] extern crate std;` if needed for host tests in this `no_std` crate
  - Add `#[cfg(test)] mod tests;`
- Create `exbuddy/src/tests.rs`
  - Own all allocator tests outside `pfn.rs`
  - Provide page-size matrix helpers
  - Provide aligned section memory buffers
  - Provide shadow model and operation helpers
  - Provide constructed boundary tests
  - Provide random pressure tests
- Keep `exbuddy/src/pfn.rs`
  - Do not remove or alter existing `pfn` tests unless formatting requires no behavioral change
- Do not modify allocator implementation files unless compilation reveals a public API mismatch in the tests

---

## Task 1: Register the Allocator Test Module

**Files:**
- Modify: `exbuddy/src/lib.rs`
- Create: `exbuddy/src/tests.rs`

- [ ] **Step 1: Add the test module hook**

In `exbuddy/src/lib.rs`, add test-only std linkage and module registration near the crate/module declarations:

```rust
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
```

Keep `pub mod pfn;` unchanged so the existing `pfn` tests remain in `exbuddy::pfn`.

- [ ] **Step 2: Create a minimal tests module**

Create `exbuddy/src/tests.rs` with imports and a smoke test:

```rust
use memory_addr::pa;

use crate::{BuddyAllocator, BuddyError};

fn run_for_page_sizes(mut case: impl FnMut(usize)) {
    for page_size_shift in [4usize, 5usize] {
        case(page_size_shift);
    }
}

#[test]
fn initialization_and_section_boundaries() {
    run_for_page_sizes(|page_size_shift| {
        let mut allocator = BuddyAllocator::new();
        assert!(!allocator.is_initialized());
        assert_eq!(allocator.ensure_initialized(), Err(BuddyError::NotInitialized));

        unsafe { allocator.init(page_size_shift, 0).unwrap() };
        assert!(allocator.is_initialized());
        assert_eq!(allocator.ensure_initialized(), Ok(()));

        assert_eq!(allocator.find_section_by_addr(pa!(0)), None);
    });
}
```

- [ ] **Step 3: Run the initial focused test**

Run: `cargo test -p exbuddy initialization_and_section_boundaries -- --nocapture`

Expected: PASS. If `std` linkage is not needed, keeping `extern crate std` is still acceptable under `#[cfg(test)]`.

- [ ] **Step 4: Format touched files**

Run: `cargo fmt -p exbuddy`

Expected: formatting succeeds.

---

## Task 2: Build Section Memory and Harness Helpers

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Add aligned owned memory buffers**

Add a `SectionMemory` helper. It must own the backing allocation so ranges passed to `add_section` stay valid for the allocator lifetime.

```rust
use core::mem;

use memory_addr::{MemoryAddr, PhysAddrRange};
use std::{vec, vec::Vec};

struct SectionMemory {
    storage: Vec<u8>,
    start: usize,
    bytes: usize,
}

impl SectionMemory {
    fn new(page_size_shift: usize, total_pages: usize, misalign: usize) -> Self {
        let page_size = 1usize << page_size_shift;
        let bytes = total_pages << page_size_shift;
        let mut storage = vec![0u8; bytes + page_size + misalign + mem::align_of::<usize>()];
        let base = storage.as_mut_ptr() as usize + misalign;
        let start = align_up_usize(base, page_size);
        Self { storage, start, bytes }
    }

    fn range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(pa!(self.start), self.bytes)
    }
}

fn align_up_usize(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}
```

If the compiler warns that `storage` is unread, keep the field; ownership is the point.

- [ ] **Step 2: Add layout sizing helpers**

Add helpers that compute total pages from actual public layout, accounting for metadata overhead:

```rust
use crate::{BuddyAllocator, BuddySection, MAX_ORDER};

fn total_pages_for_heap_at_least(page_size_shift: usize, min_heap_pages: usize) -> usize {
    let mut total_pages = min_heap_pages + 1;
    loop {
        let layout = BuddySection::layout_for_section(total_pages, page_size_shift)
            .expect("layout should eventually fit");
        if layout.heap_pages >= min_heap_pages {
            assert!(layout.heap_pages <= BuddyAllocator::MAX_HEAP_PAGES_IN_SECTION);
            return total_pages;
        }
        total_pages = total_pages.saturating_add((min_heap_pages - layout.heap_pages).max(1));
    }
}

fn huge_total_pages(page_size_shift: usize) -> usize {
    total_pages_for_heap_at_least(page_size_shift, (1usize << MAX_ORDER) * 2 + 1)
}
```

- [ ] **Step 3: Add a reusable initialized fixture**

Create a fixture that initializes the allocator and adds multiple sections:

```rust
struct AllocatorFixture {
    allocator: BuddyAllocator,
    sections: Vec<SectionMemory>,
}

impl AllocatorFixture {
    fn with_standard_sections(page_size_shift: usize) -> Self {
        let mut allocator = BuddyAllocator::new();
        unsafe { allocator.init(page_size_shift, 0).unwrap() };

        let section_page_counts = [
            total_pages_for_heap_at_least(page_size_shift, 3),
            total_pages_for_heap_at_least(page_size_shift, 257),
            total_pages_for_heap_at_least(page_size_shift, 4099),
            huge_total_pages(page_size_shift),
        ];

        let mut sections = Vec::new();
        for (index, total_pages) in section_page_counts.into_iter().enumerate() {
            let section = SectionMemory::new(page_size_shift, total_pages, index * 7);
            unsafe { allocator.add_section(section.range()).unwrap() };
            sections.push(section);
        }

        Self { allocator, sections }
    }
}
```

If accidental host address overlap occurs because buffers are adjacent in memory, allocate and add one section at a time only after confirming `add_section` succeeds. Do not loosen overlap assertions.

- [ ] **Step 4: Extend `initialization_and_section_boundaries` to use the fixture**

Assert:

```rust
let fixture = AllocatorFixture::with_standard_sections(page_size_shift);
assert_eq!(fixture.allocator.section_count(), 4);
let stats = fixture.allocator.stats();
assert!(stats.heap_pages() > (1usize << MAX_ORDER) * 2);
assert_eq!(stats.free_pages(), stats.heap_pages());
```

Also assert every section from `section_iter()` has `stats.free_pages == stats.heap_pages` and `heap_region` lies inside `region`.

- [ ] **Step 5: Run the harness test**

Run: `cargo test -p exbuddy initialization_and_section_boundaries -- --nocapture`

Expected: PASS. If memory usage is excessive, reduce medium section sizes first, not the huge-section lower bound.

---

## Task 3: Implement the Shadow Model Core

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Add model data structures**

Add section and allocation records. Keep them test-only and simple.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiFamily {
    Block,
    Frames,
    Frame,
    BlocksAt,
    FramesAt,
}

#[derive(Debug, Clone)]
struct AllocationRecord {
    addr: usize,
    pages: usize,
    order: usize,
    family: ApiFamily,
}

#[derive(Debug)]
struct ModelSection {
    heap_start: usize,
    heap_pages: usize,
    free: Vec<bool>,
}

#[derive(Debug)]
struct ShadowModel {
    page_size_shift: usize,
    sections: Vec<ModelSection>,
    allocations: Vec<AllocationRecord>,
}
```

Use `free: Vec<bool>` with `true` meaning free. It is memory-heavy for the huge section but acceptable for the required default test size and avoids bit-level helpers. Keep huge-section buffers to the smallest total page count whose public layout actually satisfies the `heap_pages > 2 * (1 << MAX_ORDER)` requirement.

- [ ] **Step 2: Build the model from public section iteration**

```rust
impl ShadowModel {
    fn from_allocator(allocator: &BuddyAllocator, page_size_shift: usize) -> Self {
        let sections = allocator
            .section_iter()
            .map(|section| ModelSection {
                heap_start: section.heap_region.start.as_usize(),
                heap_pages: section.stats.heap_pages,
                free: vec![true; section.stats.heap_pages],
            })
            .collect();

        Self { page_size_shift, sections, allocations: Vec::new() }
    }
}
```

- [ ] **Step 3: Add range lookup and mutation helpers**

Implement helpers:

```rust
impl ShadowModel {
    fn page_size(&self) -> usize { 1usize << self.page_size_shift }

    fn locate_range(&self, addr: usize, pages: usize) -> Option<(usize, usize)> { /* section index, page index */ }

    fn range_is_free(&self, section_index: usize, page_index: usize, pages: usize) -> bool { /* all free */ }

    fn mark_allocated(&mut self, addr: usize, pages: usize, order: usize, family: ApiFamily) { /* set false, push record */ }

    fn mark_free(&mut self, addr: usize, pages: usize) { /* set true, remove matching or containing record as appropriate */ }
}
```

Use page arithmetic only after checking alignment and heap containment.

- [ ] **Step 4: Add stats oracle**

Compare model state with allocator stats after every operation:

```rust
impl ShadowModel {
    fn assert_stats_match(&self, allocator: &BuddyAllocator) {
        let mut expected_heap = 0usize;
        let mut expected_free = 0usize;
        for section in &self.sections {
            expected_heap += section.heap_pages;
            expected_free += section.free.iter().filter(|&&free| free).count();
        }

        let stats = allocator.stats();
        assert_eq!(stats.heap_pages, expected_heap);
        assert_eq!(stats.free_pages, expected_free);

        for (index, section) in self.sections.iter().enumerate() {
            let stats = allocator.section_stats(index).unwrap();
            assert_eq!(stats.heap_pages, section.heap_pages);
            assert_eq!(stats.free_pages, section.free.iter().filter(|&&free| free).count());
        }
    }
}
```

- [ ] **Step 5: Add checkpoint page-state oracle**

```rust
impl ShadowModel {
    fn assert_pages_match(&self, allocator: &BuddyAllocator) {
        let page_size = self.page_size();
        for section in &self.sections {
            for page in 0..section.heap_pages {
                let addr = pa!(section.heap_start + page * page_size);
                let expected_allocated = !section.free[page];
                assert_eq!(allocator.is_allocated(addr), Ok(expected_allocated));
                assert_eq!(allocator.is_allocated(addr + (page_size - 1)), Ok(expected_allocated));
            }
        }
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p exbuddy initialization_and_section_boundaries -- --nocapture`

Expected: PASS. This task mostly adds helpers, so the existing test remains the compile check.

---

## Task 4: Add Constructed Section and Error Boundary Tests

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Expand `initialization_and_section_boundaries`**

Add assertions for invalid initialization and basic uninitialized operation behavior:

```rust
let mut allocator = BuddyAllocator::new();
assert_eq!(unsafe { allocator.init(0, 0) }, Err(BuddyError::InvalidPageSize));
assert_eq!(allocator.ensure_initialized(), Err(BuddyError::NotInitialized));
assert_eq!(allocator.alloc_frame(), Err(BuddyError::NoMemory));
```

Note: `alloc_frame` on an uninitialized allocator currently searches no sections and returns `NoMemory`; assert current public behavior rather than inventing a new requirement.

- [ ] **Step 2: Implement `add_section_layout_and_overlap_errors`**

Add a test covering:

```rust
#[test]
fn add_section_layout_and_overlap_errors() {
    run_for_page_sizes(|page_size_shift| {
        let page_size = 1usize << page_size_shift;
        let mut allocator = BuddyAllocator::new();
        unsafe { allocator.init(page_size_shift, 0).unwrap() };

        let too_small = SectionMemory::new(page_size_shift, 1, 0);
        assert_eq!(unsafe { allocator.add_section(too_small.range()) }, Err(BuddyError::SectionTooSmall));

        let valid = SectionMemory::new(page_size_shift, total_pages_for_heap_at_least(page_size_shift, 32), 0);
        let valid_range = valid.range();
        let unaligned = PhysAddrRange::from_start_size(valid_range.start + 1, valid_range.size() - 1);
        assert_eq!(unsafe { allocator.add_section(unaligned) }, Err(BuddyError::NotAligned));

        assert_eq!(allocator.check_metadata_overlap(valid_range, valid_range), Ok(true));
        unsafe { allocator.add_section(valid_range).unwrap() };
        assert_eq!(unsafe { allocator.add_section(valid_range) }, Err(BuddyError::SectionOverlap));

        let metadata_end = valid_range.start + page_size;
        assert_eq!(allocator.check_metadata_overlap(valid_range, PhysAddrRange::new(metadata_end, metadata_end + page_size)), Ok(false));

        drop(valid);
    });
}
```

Keep every `SectionMemory` backing allocation alive for as long as the allocator may reference that section. Do not drop or move section backing storage until after the allocator is no longer used.

- [ ] **Step 3: Run focused section tests**

Run: `cargo test -p exbuddy add_section_layout_and_overlap_errors -- --nocapture`

Expected: PASS.

Run: `cargo test -p exbuddy initialization_and_section_boundaries -- --nocapture`

Expected: PASS.

---

## Task 5: Add Constructed Allocation and Free Boundary Tests

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Implement model wrappers for placement allocation**

Add helper methods that call allocator APIs and validate returned addresses against the model:

```rust
impl ShadowModel {
    fn alloc_block_checked(&mut self, allocator: &mut BuddyAllocator, order: usize, align: usize) -> Result<usize, BuddyError> {
        let result = allocator.alloc_block(order, align);
        match result {
            Ok(addr) => {
                let addr_usize = addr.as_usize();
                let pages = 1usize << order;
                assert_eq!(addr_usize % self.page_size(), 0);
                assert_eq!(addr_usize % align, 0);
                let (section, page) = self.locate_range(addr_usize, pages).expect("allocated block in heap");
                assert!(self.range_is_free(section, page, pages));
                self.mark_allocated(addr_usize, pages, order, ApiFamily::Block);
                Ok(addr_usize)
            }
            Err(error) => Err(error),
        }
    }

    fn alloc_frames_checked(&mut self, allocator: &mut BuddyAllocator, count: usize, align: usize) -> Result<usize, BuddyError> {
        let result = allocator.alloc_frames(count, align);
        match result {
            Ok(addr) => {
                let order = crate::page_count_to_order_ceiling(count).unwrap();
                let pages = 1usize << order;
                let addr_usize = addr.as_usize();
                let (section, page) = self.locate_range(addr_usize, pages).expect("allocated frames in heap");
                assert!(self.range_is_free(section, page, pages));
                self.mark_allocated(addr_usize, pages, order, ApiFamily::Frames);
                Ok(addr_usize)
            }
            Err(error) => Err(error),
        }
    }
}
```

Do not require a unique chosen address.

- [ ] **Step 2: Implement matching free wrappers**

Add wrappers for `dealloc_block`, `dealloc_frames`, and `dealloc_frame` that update model state only after allocator success.

- [ ] **Step 3: Implement `alloc_block_and_frame_boundaries`**

Test:

- invalid order `MAX_ORDER + 1`
- invalid alignments `0`, `page_size / 2`, `page_size + 1`
- orders `0..=MAX_ORDER` where the model can validate success or `NoMemory`
- `alloc_frame` followed by `dealloc_frame`
- `alloc_frames` for counts `[0, 1, 2, 3, 4, 7, 8, 9, (1 << MAX_ORDER), (1 << MAX_ORDER) + 1]`

After every operation call `model.assert_stats_match(&allocator)`. At the end call `model.assert_pages_match(&allocator)`.

- [ ] **Step 4: Implement `exhaustion_and_reuse_patterns`**

Repeatedly allocate varied small blocks until `NoMemory`, free every third allocation, allocate varied medium blocks, then free all remaining records in reverse and shuffled order. Use model wrappers throughout.

- [ ] **Step 5: Run allocation boundary tests**

Run: `cargo test -p exbuddy alloc_block_and_frame_boundaries -- --nocapture`

Expected: PASS.

Run: `cargo test -p exbuddy exhaustion_and_reuse_patterns -- --nocapture`

Expected: PASS.

---

## Task 6: Add Address-Specific Allocation Tests

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Implement model wrappers for `*_at` APIs**

Add wrappers:

```rust
impl ShadowModel {
    fn expected_alloc_at_error(&self, addr: usize, pages: usize) -> Option<BuddyError> { /* invalid count, alignment, not in one heap, already allocated */ }

    fn alloc_blocks_at_checked(&mut self, allocator: &mut BuddyAllocator, addr: usize, pages: usize) {
        let expected_error = self.expected_alloc_at_error(addr, pages);
        let result = allocator.alloc_blocks_at(pa!(addr), pages);
        match (expected_error, result) {
            (None, Ok(())) => self.mark_allocated(addr, pages, crate::page_count_to_order_floor(pages).unwrap_or(0), ApiFamily::BlocksAt),
            (Some(expected), Err(actual)) => assert_eq!(actual, expected),
            other => panic!("alloc_blocks_at mismatch: {other:?}"),
        }
        self.assert_stats_match(allocator);
    }
}
```

For successful `*_at` range allocations, do not assume one internal block. Mark all pages allocated and store the range record with the original page count.

- [ ] **Step 2: Implement deallocation wrappers for `dealloc_blocks_at` and `dealloc_frames_at`**

The expected success condition is stricter than page allocated state: the `(addr, pages)` pair must match a previous successful at-range record or a set of whole allocated blocks accepted by current public behavior. For constructed tests, use exact matching records for success and deliberately mismatched ranges for expected `OrderMismatch` or `NotAllocated`.

- [ ] **Step 3: Implement `alloc_at_non_natural_boundaries`**

Build ranges from public section heap starts:

- `heap_start + 1 * page_size`, `3` pages
- `heap_start + 5 * page_size`, `17` pages
- `heap_start + ((1 << 10) - 3) * page_size`, `19` pages
- `heap_start + ((1 << MAX_ORDER) - 5) * page_size`, `11` pages when the huge section contains it

Allocate them with `alloc_blocks_at`, assert stats, assert checkpoint pages, then deallocate in a different order with `dealloc_blocks_at`.

- [ ] **Step 4: Implement `dealloc_mismatch_and_error_paths`**

Cover:

- `alloc_blocks_at` with count `0` returns `InvalidPageCount`
- unaligned address returns `NotAligned`
- outside heap returns `NotInHeap`
- range crossing section end returns `NotInHeap`
- overlapping allocation returns `AlreadyAllocated`
- partial free of an at-range returns `OrderMismatch` or current public error
- double free returns `NotAllocated` or current public error

Use current observed public errors where source code dictates the order.

- [ ] **Step 5: Run address-specific tests**

Run: `cargo test -p exbuddy alloc_at_non_natural_boundaries -- --nocapture`

Expected: PASS.

Run: `cargo test -p exbuddy dealloc_mismatch_and_error_paths -- --nocapture`

Expected: PASS.

---

## Task 7: Add Random Generator and Pressure Tests

**Files:**
- Modify: `exbuddy/src/tests.rs`

- [ ] **Step 1: Add seed and PRNG helpers**

Use no new dependencies:

```rust
use std::{env, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self { Self(seed | 1) }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn usize(&mut self, upper: usize) -> usize {
        if upper == 0 { 0 } else { (self.next_u64() as usize) % upper }
    }
}

fn test_seed(test_name: &str, page_size_shift: usize) -> u64 {
    if let Ok(seed) = env::var("EXBUDDY_TEST_SEED") {
        return seed.parse().expect("EXBUDDY_TEST_SEED must be u64");
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    nanos ^ ((page_size_shift as u64) << 32) ^ hash_name(test_name)
}
```

Add a simple `hash_name` over bytes.

- [ ] **Step 2: Add operation description logging**

Create `Operation` enum with `Debug`, and wrap random phases so panics include:

- test name
- seed
- page size shift
- phase
- operation index
- operation debug value

A simple `assert_with_context` helper or `panic!("seed=... op={op:?}")` is enough.

- [ ] **Step 3: Implement random operation generation**

Generate biased operations from model state:

- choose existing allocation records for frees about half the time
- choose heap boundaries, section gaps, existing record starts/ends, and random addresses for at-range APIs
- choose counts around powers of two and random small/medium values
- choose orders `0..=MAX_ORDER + 1`
- choose alignments from `[page_size / 2, page_size, page_size * 2, page_size * 3, 1 << random]`

- [ ] **Step 4: Implement `randomized_block_api_pressure`**

Use only block/frame placement APIs and matching frees. Run several phases, for example 4 phases with 500 operations each per page size. After each operation check stats. After each phase run `assert_pages_match`.

- [ ] **Step 5: Implement `randomized_at_api_pressure`**

Use only `alloc_blocks_at`, `dealloc_blocks_at`, `alloc_frames_at`, and `dealloc_frames_at`. Bias toward non-natural boundaries and existing fragmented areas. Run several phases and checkpoint after each phase.

- [ ] **Step 6: Implement `randomized_mixed_api_pressure`**

Interleave all public allocation and deallocation methods. Free records only through semantically matching APIs unless the generated operation is intentionally testing a mismatch error. Run several phases and checkpoint after each phase.

- [ ] **Step 7: Run random tests with a fixed replay seed first**

Run: `EXBUDDY_TEST_SEED=1 cargo test -p exbuddy randomized_ -- --nocapture`

Expected: PASS and printed/logged context includes seed `1` if a failure occurs.

- [ ] **Step 8: Run random tests with fresh seeds**

Run: `cargo test -p exbuddy randomized_ -- --nocapture`

Expected: PASS.

---

## Task 8: Remove Superseded Allocator Tests and Verify Full Suite

**Files:**
- Modify: any `exbuddy/src/*.rs` file containing allocator tests outside `pfn.rs`
- Keep: `exbuddy/src/pfn.rs` tests
- Modify: `exbuddy/src/tests.rs` only if verification reveals test issues

- [ ] **Step 1: Search for remaining allocator tests outside `pfn.rs`**

Use the code search tool for:

- `#[cfg(test)]`
- `mod tests`
- `#[test]`

Expected: allocator tests exist only in `exbuddy/src/tests.rs`; `exbuddy/src/pfn.rs` still contains its `pfn` tests.

- [ ] **Step 2: Remove superseded tests outside `pfn.rs`**

If any old allocator tests remain in `buddy.rs`, `section.rs`, `page_meta.rs`, `stats.rs`, `error.rs`, or `lib.rs`, remove them. Do not remove doctest examples or `pfn.rs` tests.

- [ ] **Step 3: Run formatting**

Run: `cargo fmt -p exbuddy`

Expected: PASS.

- [ ] **Step 4: Run the exbuddy test suite**

Run: `cargo test -p exbuddy -- --nocapture`

Expected: PASS, including `pfn` tests and the new allocator tests.

- [ ] **Step 5: Run workspace formatting check if available**

Run: `cargo fmt --check`

Expected: PASS.

- [ ] **Step 6: Review the final diff**

Run: `git diff -- exbuddy/src/lib.rs exbuddy/src/tests.rs exbuddy/src/pfn.rs docs/superpowers/specs/2026-06-02-exbuddy-tests-design.md docs/superpowers/plans/2026-06-02-exbuddy-tests.md`

Expected:

- `pfn.rs` tests are still present
- new tests use only public allocator APIs
- random tests support `EXBUDDY_TEST_SEED`
- every allocator test runs for page shifts 4 and 5
- huge section sizing is based on actual `layout_for_section`

---

## Implementation Notes

- This is test code, so using `std::vec::Vec`, `std::println!`, environment variables, and time is acceptable under `#[cfg(test)]`.
- Keep the allocator implementation unchanged unless tests reveal an actual bug and the user agrees to fix behavior as part of the same work.
- Do not assert private `PageMeta` states, free-list internals, or visitor internals. All allocator-state assertions must use public allocator methods, public section fields, public stats, and public `is_allocated`.
- If a modeled expected error disagrees with source-defined public behavior, update the model to match public behavior, then add a small constructed assertion documenting that behavior. For current `*_at` behavior, expect `AlreadyAllocated` for allocating over non-free pages and `OrderMismatch` for deallocating a range that cuts through allocated block boundaries.
- Random tests should not depend on operation count being huge to find basic bugs. Constructed tests carry deterministic boundary coverage; random tests carry state-space coverage.
- Do not add `rand` or other dependencies unless the in-test PRNG proves insufficient.
