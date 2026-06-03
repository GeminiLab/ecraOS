# exbuddy Allocator Test Rewrite Design

**Goal:** Rewrite the `exbuddy` allocator tests so they validate the buddy allocator with gray-box knowledge of its public behavior and source-level invariants, while calling only public APIs. Existing tests outside `exbuddy::pfn` will be removed. The `pfn` module tests remain in place.

**Scope:** This design covers tests for the `exbuddy` crate. It does not change allocator behavior or add private test-only APIs.

**Tech Stack:** Rust host `cargo test`, `std` available only under `#[cfg(test)]`, `memory_addr` address types, public `exbuddy` APIs.

---

## 1. Test Module Structure

Add an independent allocator test module outside `pfn.rs`, most likely in `exbuddy/src/lib.rs` or a dedicated `exbuddy/src/tests.rs` module compiled only with `#[cfg(test)]`.

Every allocator test case runs for both tiny page sizes:

- `page_size_shift = 4`, page size 16B
- `page_size_shift = 5`, page size 32B

The test module is split into multiple focused `#[test]` functions rather than one or two large tests:

- `initialization_and_section_boundaries`
- `add_section_layout_and_overlap_errors`
- `alloc_block_and_frame_boundaries`
- `alloc_at_non_natural_boundaries`
- `dealloc_mismatch_and_error_paths`
- `exhaustion_and_reuse_patterns`
- `randomized_block_api_pressure`
- `randomized_at_api_pressure`
- `randomized_mixed_api_pressure`

Each function delegates through a shared helper, such as `run_for_page_sizes`, so both 16B and 32B page sizes receive the same coverage.

---

## 2. Test Memory Harness

Tests run on preallocated user-space memory and use `VirtAddr == PhysAddr` by initializing `BuddyAllocator` with `virt_phys_offset = 0`.

The harness owns page-aligned buffers and exposes their addresses as physical ranges through `memory_addr::pa!` or equivalent constructors. Each range is aligned to the current tiny page size before being passed to `BuddyAllocator::add_section`.

The allocator receives several sections with intentionally different sizes and placement characteristics:

- very small sections that barely fit metadata plus heap
- medium and awkward sections that produce non-round heap sizes
- medium sections intended to create different absolute PFN alignments
- one huge section whose actual heap page count exceeds `2 * (1 << MAX_ORDER)` pages

The huge section size is computed by asking `BuddySection::layout_for_section(total_pages, page_size_shift)` for the actual heap page count and increasing `total_pages` until the resulting `heap_pages` exceeds `2 * (1 << MAX_ORDER)`, preferably near `2.5 * (1 << MAX_ORDER)`. The computed heap size must also remain no larger than `BuddyAllocator::MAX_HEAP_PAGES_IN_SECTION`, otherwise `add_section` would reject the section. This is important because 16B and 32B pages make metadata a large part of the total section size. The test must not estimate huge-section suitability from raw buffer bytes alone.

All sections are added through public allocator APIs. The tests may inspect public section iteration, public section fields, and public stats, but must not access private metadata, free-list nodes, page flags, or visitor internals.

---

## 3. Shadow Model and Oracles

The test harness maintains a page-granular shadow model for every public heap range. The model tracks:

- section heap ranges and page size
- per-page allocation state
- allocation records with address, page count, order/count, size, and API family
- expected per-section and aggregate stats

The model is the oracle for operation results.

For address-specific APIs, such as `alloc_blocks_at` and `alloc_frames_at`, the model predicts whether the requested page range is valid, aligned, in one section heap, fully free, or should fail with the expected public error.

For placement APIs, such as `alloc_block`, `alloc_frames`, and `alloc_frame`, the model must not predict a unique address. Instead, it predicts whether at least one legal candidate exists. If allocation succeeds, the model validates the returned address:

- the address is page-aligned
- the address lies wholly inside one managed heap range
- the address satisfies the requested byte alignment and byte alignment constraint
- the returned block or rounded-up frame range is fully free in the model before being marked allocated
- the allocation size and stats delta match the public API semantics, including `alloc_frames` allocating the full block from `page_count_to_order_ceiling(count)` rather than only the requested count

After every operation, tests compare `allocator.stats()` and `allocator.section_stats(index)` with the model's expected totals. At random phase checkpoints, the tests also scan page state with public `is_allocated` calls and compare every heap page against the model. Full page-state scans are intentionally checkpointed rather than done after every operation because the huge tiny-page sections contain millions of heap pages.

Random tests support seed replay from the beginning, for example through an environment variable. By default each run still uses a fresh seed. When a random test fails, it prints the seed, page size, phase, operation index, and operation details so the failing run can be rerun exactly.

---

## 4. Constructed Boundary Scenarios

The hand-crafted tests validate edge cases that random generation should hit often but should not be trusted to hit deterministically.

Initialization and section tests cover:

- operations before `init` return `NotInitialized` where applicable
- `init(0, ...)` returns `InvalidPageSize`
- section ranges that are unaligned, too small, overlapping, and valid
- `check_metadata_overlap` for overlapping and non-overlapping metadata query ranges
- `section_count`, `section_iter`, `section_stats`, and aggregate `stats`

Allocation boundary tests cover:

- `alloc_block` for orders from 0 through `MAX_ORDER` when the section can supply it
- invalid orders greater `MAX_ORDER`
- invalid alignments, including non-power-of-two and smaller than page size
- `alloc_frame`, `alloc_frames`, and `alloc_block` equivalence where public semantics overlap
- `alloc_frames` counts around `0`, `1`, powers of two, powers of two minus one, and the `MAX_ORDER` limit
- exhaustion, patterned frees, and reuse after merging

Address-specific tests cover repeated ranges that intentionally do not line up with natural buddy block boundaries:

- starts inside a larger free block
- ends inside a larger free block
- both start and end inside a larger free block
- ranges crossing internal split boundaries created by previous allocations
- overlapping already-allocated ranges
- partial frees and mismatched `dealloc_blocks_at` / `dealloc_frames_at` counts
- unaligned addresses
- addresses outside any heap
- ranges crossing a section edge or section gap

These scenarios repeatedly allocate and free different ranges rather than repeating one single allocation pattern.

---

## 5. Random Pressure Tests

Random pressure is the main coverage mechanism. Tests use a fresh seed on every run, preferably derived from system time plus process entropy. Randomness is not fixed by default.

The random tests are split by target API family:

- `randomized_block_api_pressure` biases toward `alloc_block`, `dealloc_block`, `alloc_frame`, `dealloc_frame`, `alloc_frames`, and `dealloc_frames`
- `randomized_at_api_pressure` biases toward `alloc_blocks_at`, `dealloc_blocks_at`, `alloc_frames_at`, and `dealloc_frames_at`
- `randomized_mixed_api_pressure` interleaves all allocation and deallocation methods in arbitrary order while preserving matching records for successful frees

Each random test runs multiple phases. Each phase performs hundreds to thousands of operations with varied parameters and then runs a checkpoint page-state scan. Generation is biased toward:

- heap starts and ends
- section gaps
- section boundaries
- alignment boundaries
- powers of two, powers of two plus one, and powers of two minus one counts
- maximum legal and illegal orders/counts
- non-power-of-two counts
- fragmented free/allocated neighborhoods
- ranges that begin or end near existing allocation records

The generator should produce both expected-success and expected-failure operations. Failure paths are part of the coverage and must be checked against the expected public `BuddyError`.

---

## 6. Public API Coverage

The rewritten tests cover these public APIs directly:

- `BuddyAllocator::new`
- `BuddyAllocator::init`
- `BuddyAllocator::is_initialized`
- `BuddyAllocator::ensure_initialized`
- `BuddyAllocator::add_section`
- `BuddyAllocator::check_metadata_overlap`
- `BuddyAllocator::section_count`
- `BuddyAllocator::section_iter`
- `BuddyAllocator::find_section_by_addr`
- `BuddyAllocator::section_stats`
- `BuddyAllocator::stats`
- `BuddyAllocator::alloc_block`
- `BuddyAllocator::dealloc_block`
- `BuddyAllocator::alloc_blocks_at`
- `BuddyAllocator::dealloc_blocks_at`
- `BuddyAllocator::alloc_frames`
- `BuddyAllocator::alloc_frame`
- `BuddyAllocator::dealloc_frames`
- `BuddyAllocator::dealloc_frame`
- `BuddyAllocator::alloc_frames_at`
- `BuddyAllocator::dealloc_frames_at`
- `BuddyAllocator::is_allocated`
- `BuddySection::layout_for_section`
- `BuddySection::metadata_size`
- public section stats and heap range fields available through `section_iter`

The tests may also continue to rely on the existing `page_count_to_order_*` doctests and `pfn` tests. They should not remove `pfn` module tests.

---

## 7. Completion Criteria

The rewrite is complete when:

- allocator tests outside `pfn` have been replaced by the new independent module
- `pfn.rs` tests remain intact
- every allocator test runs for both 16B and 32B page sizes
- huge-section tests verify actual `heap_pages > 2 * (1 << MAX_ORDER)` despite metadata overhead
- all allocation and deallocation methods are tested individually and in combination
- random pressure tests use a new seed by default, support explicit seed replay, and report enough context to diagnose failures
- all tests pass with `cargo test -p exbuddy`
- formatting passes for touched Rust files
