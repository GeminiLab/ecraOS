# Buddy-Slab Allocator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement buddy page allocator (exbuddy) and slab allocator (exslab) as separate crates, then integrate them into ecraOS to enable `#[global_allocator]` and `alloc` crate types.

**Architecture:** Two standalone `#![no_std]` crates — `exbuddy` manages physical pages via buddy system, `exslab` manages small objects via per-CPU slab caches. They connect through a `PageProvider` trait implemented in the `ecraos::mem::alloc` integration module. Both use runtime page size (not const generics) and `kspin::SpinNoIrq` for synchronization.

**Tech Stack:** Rust `#![no_std]`, `memory_addr` for address types, `kspin` for spinlocks, `kernel_guard` (transitively via kspin). Reference implementation at `refs/buddy-slab-allocator/`.

**Spec:** `docs/superpowers/specs/2026-05-16-buddy-slab-allocator-design.md`

---

## File Structure

### New files to create:

```
exbuddy/
├── Cargo.toml
└── src/
    ├── lib.rs          # Crate root, re-exports
    ├── error.rs        # AllocError, AllocResult
    ├── page_meta.rs    # PageMeta, PageFlags, PFN_NONE, free_list ops
    └── buddy.rs        # BuddySection, BuddyAllocator, AllocatorUsage

exslab/
├── Cargo.toml
└── src/
    ├── lib.rs          # Crate root, re-exports, PageProvider trait
    ├── error.rs        # AllocError, AllocResult
    ├── size_class.rs   # SizeClass enum, SIZE_CLASS_COUNT, SLAB_MAX_SIZE
    ├── page.rs         # SlabPageHeader, SLAB_MAGIC, bitmap/remote-free ops
    ├── cache.rs        # SlabCache (partial/full/empty lists), CacheDeallocResult
    └── slab.rs         # SlabAllocator, SlabAllocResult, SlabDeallocResult,
                        # SlabPoolDeallocResult, SlabPoolTrait, PerCpuSlab, StaticSlabPool

ecraos/src/mem/
└── alloc/
    ├── mod.rs          # EcraosAllocator, GlobalAlloc, PageProvider impl, init_allocators(), public API
    └── test.rs         # QEMU smoke tests, removable after verification
```

### Files to modify:

```
Cargo.toml                          # Add exbuddy, exslab to workspace members
ecraos/Cargo.toml                   # Add exbuddy, exslab dependencies
ecraos/src/main.rs                  # Call init_allocators() in kernel_entry_with_vmm
ecraos/src/mem.rs                   # Add alloc module declaration
```

---

### Task 1: Create exbuddy crate skeleton + error types + page_meta

**Files:**
- Create: `exbuddy/Cargo.toml`
- Create: `exbuddy/src/lib.rs`
- Create: `exbuddy/src/error.rs`
- Create: `exbuddy/src/page_meta.rs`

- [ ] **Step 1: Create Cargo.toml**

Create `exbuddy/Cargo.toml`:
```toml
[package]
name = "exbuddy"
version = "0.1.0"
edition = "2024"

[dependencies]
memory_addr.workspace = true
```

- [ ] **Step 2: Create error.rs**

Create `exbuddy/src/error.rs`. Copy from `refs/buddy-slab-allocator/src/error.rs` but remove `NotAllocated` variant (not needed by buddy). Use the variants from the spec: `InvalidParam`, `AlreadyInitialized`, `MemoryOverlap`, `NoMemory`, `NotInitialized`, `NotFound`.

- [ ] **Step 3: Create page_meta.rs**

Create `exbuddy/src/page_meta.rs`. Copy from `refs/buddy-slab-allocator/src/buddy/page_meta.rs` with minimal changes (just the struct, const, and free_list functions). No changes needed — this file is self-contained and doesn't reference PAGE_SIZE.

- [ ] **Step 4: Create lib.rs**

Create `exbuddy/src/lib.rs`:
```rust
//! Buddy page allocator.

#![no_std]

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod page_meta;
pub use page_meta::{PageFlags, PageMeta, PFN_NONE};

pub mod buddy;
pub use buddy::{AllocatorUsage, BuddyAllocator, ManagedSection};
```

- [ ] **Step 5: Add exbuddy to workspace**

Modify workspace `Cargo.toml` to add `"exbuddy"` to `workspace.members`.

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p exbuddy`
Expected: FAIL — `buddy.rs` not yet created. But `error.rs` and `page_meta.rs` should have no issues.

Create a stub `exbuddy/src/buddy.rs` with empty structs to make it compile:
```rust
//! Buddy allocator implementation.

pub struct BuddyAllocator;
pub struct ManagedSection;
pub struct AllocatorUsage {
    pub total_pages: usize,
    pub used_pages: usize,
}
```

Run: `cargo check -p exbuddy`
Expected: PASS

- [ ] **Step 7: Commit**

```
git add exbuddy/
git commit -m "feat(exbuddy): create crate skeleton with error types and page metadata"
```

---

### Task 2: Implement BuddySection + BuddyAllocator init and add_region

**Files:**
- Modify: `exbuddy/src/buddy.rs` — full implementation

- [ ] **Step 1: Implement BuddySection and region layout computation**

Rewrite `exbuddy/src/buddy.rs`. Start with the data structures and `init`/`add_region` logic.

Key differences from reference (`refs/buddy-slab-allocator/src/buddy/mod.rs`):
- No `const PAGE_SIZE` generic. Instead, `BuddyAllocator` stores `page_size: usize` and `virt_phys_offset: usize`.
- `init()` takes `PhysAddrRange` + `page_size` + `virt_phys_offset`, converts to virtual internally.
- `BuddySection` stores virtual addresses (usize), not typed addresses.
- API returns `PhysAddr`, converts via `virt_phys_offset`.

Port these from the reference:
- `normalize_region()` — add `page_size` parameter instead of const generic
- `RegionLayout`, `SectionInitSpec` structs
- `BuddySection` struct and its methods: `metadata_layout_for_pages()`, `available_heap_pages()`, `can_manage_pages()`, `compute_region_layout_with_heap_align()`, `init_at()`, `contains_heap_addr()`, `summary()`
- `BuddyAllocator` struct and: `new()`, `required_meta_size()`, `reset()`, `add_region()`, `add_region_raw()`, `section_count()`, `section()`, `total_pages()`, `managed_bytes()`, `free_pages()`
- `ManagedSection` struct

For all methods that use `<const PAGE_SIZE: usize>`, change to take `page_size: usize` from `self.page_size`.

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p exbuddy`
Expected: PASS (alloc/dealloc not yet implemented, but structs and init should compile)

- [ ] **Step 3: Commit**

```
git add exbuddy/
git commit -m "feat(exbuddy): implement BuddySection and BuddyAllocator init/add_region"
```

---

### Task 3: Implement allocation and deallocation

**Files:**
- Modify: `exbuddy/src/buddy.rs` — add alloc/dealloc methods

- [ ] **Step 1: Implement alloc_frames and dealloc_frames**

Port from reference:
- `alloc_pages()` → `alloc_frames()`: takes `count` and `align`, returns `AllocResult<PhysAddr>`. Internally works in virtual addresses, converts return value to `PhysAddr`.
- `alloc_from_section_aligned()` — change `<const PAGE_SIZE>` to use `self.page_size`
- `find_aligned_pfn_in_block()` — same change
- `dealloc_pages()` → `dealloc_frames()`: takes `PhysAddr addr`, converts to virtual for section lookup
- `dealloc_in_section()` — same logic, no PAGE_SIZE generic needed (uses section's page_size implicitly through PFN math... actually, PFN math uses `section.heap_start` and `page_size` — pass page_size as parameter)
- `find_section_by_addr()` / `find_section_by_addr_mut()` — convert PhysAddr input to virtual for comparison

Add convenience wrappers:
- `alloc_frame()` → `self.alloc_frames(1, self.page_size)`
- `dealloc_frame()` → `self.dealloc_frames(addr, 1)`

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p exbuddy`
Expected: PASS

- [ ] **Step 3: Commit**

```
git add exbuddy/
git commit -m "feat(exbuddy): implement alloc_frames, dealloc_frames, and convenience wrappers"
```

---

### Task 4: Implement alloc_frames_at, usage, set_page_flags

**Files:**
- Modify: `exbuddy/src/buddy.rs` — add remaining methods

- [ ] **Step 1: Implement alloc_frames_at**

This is new functionality (not in reference). Algorithm per spec:

1. Convert `paddr` to virtual: `vaddr = paddr.as_usize() + self.virt_phys_offset`
2. Find section containing `vaddr`
3. Compute start PFN: `(vaddr - section.heap_start) / self.page_size`
4. Validate PFN range: `start_pfn + count <= section.max_pages`
5. For each target PFN in `[start_pfn, start_pfn + count)`:
   - Find the free block containing this PFN (walk free lists from order 0 upward)
   - If the block is larger than order 0, split it until the target PFN is isolated
   - Splitting: remove block from free list, split into two halves, put non-target half back, continue splitting target half
6. Mark all target pages as `Allocated`
7. Update `section.free_pages`
8. Return `Ok(paddr)`

Key helper: `split_block_to_isolate(section, target_pfn)` — finds and splits the free block containing `target_pfn` down to order 0.

- [ ] **Step 2: Implement usage() and set_page_flags()**

Port from reference:
- `usage()` → returns `AllocatorUsage { total_pages, used_pages: total - free }`
- `set_page_flags()` → takes `PhysAddr`, converts to virtual, finds section, sets PageMeta flags

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p exbuddy`
Expected: PASS

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p exbuddy --target x86_64-unknown-none 2>&1 || true`
Fix any warnings. Note: some warnings about unused code are expected since integration isn't done yet.

- [ ] **Step 5: Commit**

```
git add exbuddy/
git commit -m "feat(exbuddy): implement alloc_frames_at, usage, and set_page_flags"
```

---

### Task 5: Create exslab crate skeleton + error types + size_class + PageProvider trait

**Files:**
- Create: `exslab/Cargo.toml`
- Create: `exslab/src/lib.rs`
- Create: `exslab/src/error.rs`
- Create: `exslab/src/size_class.rs`

- [ ] **Step 1: Create Cargo.toml**

Create `exslab/Cargo.toml`:
```toml
[package]
name = "exslab"
version = "0.1.0"
edition = "2024"

[dependencies]
kspin = "0.2"
memory_addr.workspace = true
```

- [ ] **Step 2: Create error.rs**

Create `exslab/src/error.rs`. Simpler than exbuddy — only needs `InvalidParam`, `NoMemory`, `NotInitialized` per spec.

- [ ] **Step 3: Create size_class.rs**

Create `exslab/src/size_class.rs`. Port from `refs/buddy-slab-allocator/src/slab/size_class.rs`. The file is already page-size-agnostic (takes `page_size` as runtime argument in `slab_pages()`). Copy with no changes needed.

- [ ] **Step 4: Create lib.rs with PageProvider trait and stubs**

Create `exslab/src/lib.rs`:
```rust
//! Slab allocator with per-CPU caches and lock-free remote free.

#![no_std]

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod size_class;
pub use size_class::SizeClass;

pub mod page;
pub mod cache;
pub mod slab;

use memory_addr::{PhysAddr, VirtAddr};

use crate::error::AllocResult;

/// Trait for slab to request/return pages from a backend allocator.
pub trait PageProvider: Sync {
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<VirtAddr>;
    fn dealloc_pages(&self, addr: VirtAddr, count: usize);
}
```

- [ ] **Step 5: Add exslab to workspace**

Modify workspace `Cargo.toml` to add `"exslab"` to `workspace.members`.

Create stub files for `page.rs`, `cache.rs`, `slab.rs` to make compilation work.

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p exslab`
Expected: PASS (with stubs)

- [ ] **Step 7: Commit**

```
git add exslab/
git commit -m "feat(exslab): create crate skeleton with error types, size classes, and PageProvider trait"
```

---

### Task 6: Implement SlabPageHeader

**Files:**
- Create: `exslab/src/page.rs` — full implementation

- [ ] **Step 1: Implement SlabPageHeader**

Port from `refs/buddy-slab-allocator/src/slab/page.rs`. Key changes for runtime page_size:
- `base_from_obj_addr()` loses `<const PAGE_SIZE>` generic, takes `page_size: usize` parameter instead
- `base_from_obj_addr_unknown()` takes `page_size: usize` instead of const generic
- All other methods remain the same (they work on `self` which has slab_bytes stored)

Copy these items unchanged (they don't reference PAGE_SIZE):
- `SLAB_MAGIC`, `MAX_OBJECTS_PER_SLAB`, `BITMAP_WORDS`, `MAX_SLAB_PAGES` constants
- `SlabPageHeader` struct definition
- `init()`, `data_offset()`, `data_start()`, `object_addr()`, `object_index()`
- `local_alloc()`, `local_free()`, `has_local_free()`, `is_all_free()`, `is_local_full()`
- `remote_free()`, `drain_remote_frees()`, `has_remote_frees()`
- `remote_free_object()` — takes `page_size: usize` instead of const generic

Modify:
- `base_from_obj_addr(addr: usize, slab_bytes: usize, page_size: usize)` — runtime page_size
- `base_from_obj_addr_unknown_with_page_size()` — already takes runtime page_size, keep as-is

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p exslab`
Expected: PASS

- [ ] **Step 3: Commit**

```
git add exslab/
git commit -m "feat(exslab): implement SlabPageHeader with runtime page size"
```

---

### Task 7: Implement SlabCache + SlabAllocator

**Files:**
- Create: `exslab/src/cache.rs` — full implementation
- Create: `exslab/src/slab.rs` — full implementation

- [ ] **Step 1: Implement SlabCache**

Port from `refs/buddy-slab-allocator/src/slab/cache.rs`. Key change: all `<const PAGE_SIZE: usize>` methods take `page_size: usize` parameter instead.

Modify:
- `alloc_object::<PAGE_SIZE>()` → `alloc_object(page_size: usize)`
- `try_alloc_from_partial::<PAGE_SIZE>()` → `try_alloc_from_partial(page_size: usize)`
- `dealloc_object::<PAGE_SIZE>()` → `dealloc_object(page_size: usize, obj_addr: usize)`
- `add_slab()` — unchanged (doesn't reference PAGE_SIZE directly)

- [ ] **Step 2: Implement SlabAllocator, result types, PerCpuSlab, StaticSlabPool, SlabPoolTrait**

Port from `refs/buddy-slab-allocator/src/slab/mod.rs`. Key changes:
- Remove all `<const PAGE_SIZE: usize>` generics
- `SlabAllocator` stores `page_size: usize` as field
- `PerCpuSlab` stores `page_size: usize`, wraps `SpinNoIrq<SlabAllocator>`
- `StaticSlabPool<const N: usize>` stores `page_size: usize`

Define result types per spec:
- `SlabAllocResult` — `Allocated(NonNull<u8>)` | `NeedsSlab { size_class, pages }`
- `SlabDeallocResult` — `Done` | `FreeSlab { base: VirtAddr, pages: usize }`
- `SlabPoolDeallocResult` — `Done` | `RemoteQueued` | `FreeSlab { base: VirtAddr, pages: usize }`
- `CacheDeallocResult` — `Done` | `FreeSlab { base: usize, pages: usize }` (internal, uses raw usize)

Note: `SlabPoolTrait` and `SlabPoolExt` should be defined here with VirtAddr in result types (conversion from internal usize to VirtAddr happens at this boundary).

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p exslab`
Expected: PASS

- [ ] **Step 4: Commit**

```
git add exslab/
git commit -m "feat(exslab): implement SlabCache, SlabAllocator, PerCpuSlab, and StaticSlabPool"
```

---

### Task 8: Create ecraos integration module

**Files:**
- Modify: `Cargo.toml` — ensure workspace members include exbuddy, exslab (already done in Tasks 1, 5)
- Modify: `ecraos/Cargo.toml` — add exbuddy, exslab, kspin dependencies
- Modify: `ecraos/src/mem.rs` — add `mod alloc;`
- Create: `ecraos/src/mem/alloc/mod.rs`

- [ ] **Step 1: Update ecraos/Cargo.toml**

Add dependencies:
```toml
exbuddy = { path = "../../exbuddy" }
exslab = { path = "../../exslab" }
kspin = "0.2"
```

- [ ] **Step 2: Add alloc module to mem.rs**

In `ecraos/src/mem.rs`, add `pub mod alloc;` after the existing module declarations.

- [ ] **Step 3: Create ecraos/src/mem/alloc/mod.rs**

This file contains:
- `EcraosAllocator` struct (holds `buddy: SpinNoIrq<BuddyAllocator>`, `slab_pool: &'static StaticSlabPool<1>`, `virt_phys_offset: usize`)
- `PageProvider` impl for `EcraosAllocator` (converts PhysAddr ↔ VirtAddr via stored offset)
- `unsafe impl GlobalAlloc for EcraosAllocator` (routes small→slab, large→buddy, respects lock ordering)
- `#[global_allocator]` static
- `init_allocators()` function (called from `kernel_entry_with_vmm`)
- Public interface functions: `alloc_frame()`, `alloc_frames()`, `alloc_frames_at()`, `dealloc_frames()`, `dealloc_frame()`, `usage()`

`init_allocators()` flow:
1. Get boot memory regions from `explat::mem::boot_mem_regions()`
2. Read `page_size = 1 << vmm::page_size_shift()` and `virt_phys_offset = vmm::virt_phys_offset()`
3. Init BuddyAllocator with first usable region
4. Add remaining usable regions, excluding kernel range and early allocator range
5. Create StaticSlabPool with 1 CPU (cpu_id=0)
6. Set global allocator

For the `#[global_allocator]` static, use a pattern like:
```rust
static mut ECRAOS_ALLOCATOR: Option<EcraosAllocator> = None;

#[global_allocator]
static mut GLOBAL_ALLOCATOR: &dyn GlobalAlloc = &PanickingAllocator;
```
Or use a simpler approach: a single static with `Sync` unsafe impl, initialized at boot.

Since this is `#![no_std]` kernel without lazy static, the practical approach is:
- Declare `static mut ALLOCATOR: EcraosAllocator` with `MaybeUninit` or `Option`
- After init, the GlobalAlloc impl reads from this static
- Use a separate `AtomicBool` for "initialized" check

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ecraos`
Expected: May have warnings about unused code, but no errors.

- [ ] **Step 5: Commit**

```
git add Cargo.toml ecraos/ exbuddy/ exslab/
git commit -m "feat(ecraos): integrate exbuddy and exslab with GlobalAlloc implementation"
```

---

### Task 9: Wire up init_allocators in kernel_entry_with_vmm

**Files:**
- Modify: `ecraos/src/main.rs` — call `mem::alloc::init_allocators()` after `init_vmm_later()`
- Create: `ecraos/src/mem/alloc/test.rs` — test module

- [ ] **Step 1: Call init_allocators in kernel_entry_with_vmm**

In `ecraos/src/main.rs`, in `kernel_entry_with_vmm()`, after `mem::init_vmm_later()`:
```rust
mem::alloc::init_allocators();
```

- [ ] **Step 2: Create test module stub**

Create `ecraos/src/mem/alloc/test.rs` with a `pub fn run()` that just prints "Allocator tests: TODO" for now.

In `ecraos/src/mem/alloc/mod.rs`, add `mod test;` and call `test::run()` at the end of `init_allocators()`.

- [ ] **Step 3: Build and run**

Run: `./test.sh`
Expected: Kernel boots, prints allocator init messages and test TODO, then reaches "Here we go!" and powers off.

- [ ] **Step 4: Commit**

```
git add ecraos/
git commit -m "feat(ecraos): wire up allocator initialization in kernel boot flow"
```

---

### Task 10: Implement smoke tests

**Files:**
- Modify: `ecraos/src/mem/alloc/test.rs` — full test implementation

- [ ] **Step 1: Implement buddy allocator tests**

In `test.rs`, implement `run()` with these tests using `kprintln!` for output:

```rust
// 1. alloc_frame — check alignment and range
let frame1 = alloc_frame().expect("alloc_frame failed");
assert!(frame1.as_usize() % page_size == 0);
kprintln!("  alloc_frame: OK ({:#x})", frame1);

// 2. alloc_frames(4, page_size)
let frames4 = alloc_frames(4, page_size).expect("alloc_frames(4) failed");
kprintln!("  alloc_frames(4): OK ({:#x})", frames4);

// 3. dealloc and realloc
dealloc_frame(frame1);
let frame1_again = alloc_frame().expect("alloc_frame after dealloc failed");
kprintln!("  dealloc/realloc: OK ({:#x} -> {:#x})", frame1, frame1_again);

// 4. usage
let usage = usage();
kprintln!("  usage: total={}, used={}", usage.total_pages, usage.used_pages);
assert!(usage.total_pages > 0);
assert!(usage.used_pages > 0);
```

Note: Use the public API from `super::*` (alloc_frame, alloc_frames, etc.) not the buddy directly.

- [ ] **Step 2: Implement slab/GlobalAlloc tests**

```rust
// 5. Box::new (small object, slab path)
let boxed = Box::new(42u32);
assert!(*boxed == 42);
kprintln!("  Box::new(42u32): OK");
drop(boxed);

// 6. Vec<u8>
let mut vec: Vec<u8> = Vec::new();
for i in 0..100 { vec.push(i); }
assert!(vec.len() == 100);
kprintln!("  Vec<u8> push 100: OK");
drop(vec);

// 7. Large allocation (>2048, buddy path)
let large = alloc::alloc::alloc(Layout::from_size_align(4096, 4096).unwrap());
assert!(!large.is_null());
alloc::alloc::dealloc(large, Layout::from_size_align(4096, 4096).unwrap());
kprintln!("  Large alloc (4096): OK");

// 8. String
let s = String::from("hello ecraOS");
assert!(s == "hello ecraOS");
kprintln!("  String: OK ({})", s);
drop(s);

// 9. Loop test (leak check)
let usage_before = usage();
for _ in 0..100 {
    let b = Box::new([0u8; 64]);
    drop(b);
}
let usage_after = usage();
kprintln!("  Loop 100x: before={}, after={}", usage_before.used_pages, usage_after.used_pages);
```

- [ ] **Step 3: Build and run**

Run: `./test.sh`
Expected: All test output printed to serial, kernel reaches "Here we go!" without panic.

- [ ] **Step 4: Fix any issues**

If any test panics, read the serial output, diagnose, and fix the issue in the relevant crate.

- [ ] **Step 5: Run clippy and fmt**

Run: `cargo fmt` and `cargo clippy --workspace --target x86_64-unknown-none`
Fix all warnings and format issues.

- [ ] **Step 6: Commit**

```
git add ecraos/
git commit -m "test(ecraos): add allocator smoke tests for buddy, slab, and GlobalAlloc"
```

---

### Task 11: Final cleanup and verification

**Files:**
- All files — final review pass

- [ ] **Step 1: Full clean build**

Run: `CLEAN=1 ./test.sh`
Expected: Clean build, all tests pass, kernel powers off normally.

- [ ] **Step 2: Verify code format conventions per CLAUDE.md**

Check that all items have doc comments with correct format:
- One-line summary + blank line + detailed description
- Functions: 3rd person singular present tense verb phrase
- Types: noun phrase
- Modules: noun phrase describing contents
- File item ordering: doc comments → attributes → extern crates → uses → modules → uses from self → other items

- [ ] **Step 3: Final commit (if any formatting fixes needed)**

```
git add -A
git commit -m "chore: format and documentation cleanup for allocator implementation"
```
