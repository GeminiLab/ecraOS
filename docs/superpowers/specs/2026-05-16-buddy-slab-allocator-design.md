# Buddy-Slab Allocator Design for ecraOS

## Overview

Implement a physical page allocator (buddy system) and memory allocator (slab allocator) for ecraOS, then integrate them into the kernel to enable `alloc` crate types (Box, Vec, String).

Three new components:

- **exbuddy** — standalone buddy page allocator crate
- **exslab** — standalone slab allocator crate (does not depend on exbuddy)
- **ecraos::mem::alloc** — integration module connecting both, implementing `#[global_allocator]`

## Design Decisions

| Decision | Choice |
|----------|--------|
| Crate layout | Two separate crates: `exbuddy` and `exslab` |
| Crate coupling | Trait-based decoupling via `PageProvider` trait in exslab |
| Page size | Runtime field in BuddyAllocator/SlabAllocator |
| Address types | `memory_addr::{PhysAddr, VirtAddr, PhysAddrRange}` |
| Phys/virt conversion | Fixed `virt_phys_offset` stored in BuddyAllocator |
| SMP support | SMP-ready (per-CPU slab, lock-free remote-free), single CPU initialized |
| Synchronization | `kspin::SpinNoIrq` |
| Error types | Independent `AllocError`/`AllocResult` per crate |

## Address Space Semantics

Since all physical memory is mapped into the kernel's direct mapping area at a fixed offset, phys/virt conversion is a simple addition/subtraction. The conventions for each component are:

- **BuddyAllocator internal storage**: All internal data structures (`BuddySection`, `PageMeta[]`) are accessed via virtual addresses. `BuddySection` stores `heap_start` as a virtual address and `virt_phys_offset` is stored on the `BuddyAllocator` itself. PFN arithmetic (`addr - section.heap_start / page_size`) works identically regardless of whether the base is physical or virtual since the offset cancels out.
- **BuddyAllocator public API**: All parameters and return values use `PhysAddr`. Conversion happens at the API boundary via the stored `virt_phys_offset`.
- **SlabAllocator internal storage**: `SlabPageHeader` and slab objects are at virtual addresses (required for pointer dereference). The slab allocator operates entirely in virtual address space internally.
- **PageProvider trait**: Returns `VirtAddr` (not `PhysAddr`) because the slab allocator needs virtual addresses for header access and object management. The integration layer, which implements `PageProvider`, calls the buddy allocator and converts the `PhysAddr` return value to `VirtAddr` before passing to slab.
- **ecraos public interface**: Exposes `PhysAddr` for frame-level operations.

No external `virt_to_phys()` hook (EII) is needed. All conversion is self-contained within `BuddyAllocator` via its stored offset.

## 1. exbuddy — Buddy Page Allocator

### Data Structures

**`BuddySection`** — per-region descriptor stored in the region prefix:

- `next: *mut BuddySection` — intrusive linked list pointer
- `region_start: usize`, `region_size: usize` — virtual addresses for internal use
- `meta: *mut PageMeta` — virtual pointer to PageMeta array
- `max_pages: usize`
- `heap_start: usize`, `heap_size: usize` — virtual addresses for internal use
- `free_lists: [u32; MAX_ORDER + 1]` — intrusive free list heads per order
- `free_pages: usize`, `total_pages: usize`

Internal storage uses raw `usize` (virtual addresses) for performance. The `PhysAddr`/`VirtAddr` typed conversions happen at the public API boundary.

**`PageMeta`** (~12 bytes per page):

- `flags: PageFlags` — Free / Allocated / Slab
- `order: u8` — buddy block order (meaningful only on head page)
- `prev: u32`, `next: u32` — intrusive doubly-linked list indices (PFN-based)

**`BuddyAllocator`**:

- `page_size: usize` — runtime page size
- `virt_phys_offset: usize` — fixed offset for phys/virt conversion
- `sections_head: *mut BuddySection`
- `sections_tail: *mut BuddySection`
- `section_count: usize`

**`PageFlags`**: Free, Allocated, Slab

**`MAX_ORDER`**: 20 (supports up to 4 GiB blocks with 4 KiB pages)

**`AllocatorUsage`**: `{ total_pages: usize, used_pages: usize }`. `used_pages = total_pages - free_pages`, which includes slab pages, alignment padding, and internal fragmentation.

### Initialization

`init()` receives a `PhysAddrRange`, `page_size`, and `virt_phys_offset`. The physical region is converted to virtual via `phys + virt_phys_offset`. Metadata (BuddySection + PageMeta[]) is placed in the region prefix at the virtual address. The allocator scans the heap portion from low to high, splitting into maximally-aligned buddy blocks.

`virt_phys_offset` is read from `ecraos::mem::vmm::virt_phys_offset()` at init time, which is available after `init_vmm_later()` completes.

Additional regions added via `add_region()`.

### Error Variants

```rust
pub enum AllocError {
    InvalidParam,
    AlreadyInitialized,
    MemoryOverlap,
    NoMemory,
    NotInitialized,
    NotFound,
}
```

### Public API

```rust
impl BuddyAllocator {
    pub const fn new() -> Self;

    /// Initialize over the first region. Stores page_size and virt_phys_offset.
    /// Converts the physical region to virtual for internal use.
    pub unsafe fn init(
        &mut self,
        region: PhysAddrRange,
        page_size: usize,
        virt_phys_offset: usize,
    ) -> AllocResult;

    /// Add another usable region after init.
    pub unsafe fn add_region(&mut self, region: PhysAddrRange) -> AllocResult;

    /// Allocate `count` contiguous pages with `align` alignment.
    /// Returns physical address. Internally converts to/from virtual.
    pub fn alloc_frames(&mut self, count: usize, align: usize) -> AllocResult<PhysAddr>;

    /// Allocate a single page. Convenience wrapper for alloc_frames(1, page_size).
    pub fn alloc_frame(&mut self) -> AllocResult<PhysAddr>;

    /// Allocate `count` pages at a specific physical address.
    pub unsafe fn alloc_frames_at(&mut self, paddr: PhysAddr, count: usize) -> AllocResult<PhysAddr>;

    /// Deallocate `count` contiguous pages.
    pub fn dealloc_frames(&mut self, addr: PhysAddr, count: usize);

    /// Deallocate a single page. Convenience wrapper for dealloc_frames(addr, 1).
    pub fn dealloc_frame(&mut self, addr: PhysAddr);

    /// Set page flags (used by slab to tag pages).
    pub fn set_page_flags(&mut self, addr: PhysAddr, flags: PageFlags) -> AllocResult;

    /// Return usage statistics.
    pub fn usage(&self) -> AllocatorUsage;
}
```

### alloc_frames_at Implementation

This function allocates pages at a specific physical address, which has no counterpart in the reference implementation. Algorithm:

1. Convert `paddr` to virtual address via `paddr.as_usize() + self.virt_phys_offset`
2. Find the section containing this virtual address
3. Compute the PFN: `(vaddr - section.heap_start) / page_size`
4. Check all `count` pages are free by reading their `PageMeta.flags`
5. For each page that is in a buddy block larger than one page, split the block:
   - Walk up from the target PFN's current free block
   - Split the block repeatedly until the target page is isolated
   - The approach: find the free block containing each target PFN, split it down to order 0, marking the non-target halves as free
6. Mark all target pages as `Allocated`
7. Return `paddr`

The splitting logic is more complex than normal allocation because the target may be in the middle of a large block. The implementation splits from the containing block down to individual pages, then merges back adjacent non-target pages where possible.

### Internal phys/virt Conversion

```rust
fn phys_to_virt(&self, paddr: usize) -> usize {
    paddr + self.virt_phys_offset
}

fn virt_to_phys(&self, vaddr: usize) -> usize {
    vaddr - self.virt_phys_offset
}
```

## 2. exslab — Slab Allocator

### Data Structures

**`SizeClass`**: 9 fixed size classes (8/16/32/64/128/256/512/1024/2048 bytes).

**`SlabCache`**: per-size-class cache with three intrusive lists:

- `partial` — slabs with free slots, allocation priority
- `full` — slabs with no local free slots (may have remote frees)
- `empty` — all objects free, at most 1 cached per size class

**`SlabPageHeader`**: embedded at start of each slab page:

- magic, size_class, object_count, local_free_count
- owner_cpu, slab_bytes
- list_prev/list_next
- local_bitmap (bitmask for free objects)
- remote_free_head (atomic stack), remote_free_count

**`SlabAllocator`**: manages all size class caches, `page_size` as runtime field.

**`PerCpuSlab`**: `kspin::SpinNoIrq<SlabAllocator>` wrapper with `cpu_id`.

**`StaticSlabPool<const N: usize>`**: static slab pool implementing `SlabPoolTrait`.

### Runtime Page Size Implications

The reference implementation uses `const PAGE_SIZE` generics throughout. With runtime `page_size`, all const-generic PAGE_SIZE parameters become runtime arguments. This affects:

- `SlabCache::alloc_object(page_size)` / `dealloc_object(page_size)` — page_size passed at runtime
- `SlabPageHeader::base_from_obj_addr(addr, page_size)` — page_size passed at runtime
- `SizeClass::slab_pages(page_size)` — already takes runtime argument in reference
- `SlabAllocator`, `PerCpuSlab`, `StaticSlabPool` — store page_size as field

### Error Variants

```rust
pub enum AllocError {
    InvalidParam,
    NoMemory,
    NotInitialized,
}
```

### PageProvider Trait

```rust
/// Trait for slab to request/return pages from a backend allocator.
/// Implemented by the integration layer in ecraos.
/// Returns VirtAddr because slab page headers and objects require virtual addresses
/// for pointer dereference.
pub trait PageProvider: Sync {
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<VirtAddr>;
    fn dealloc_pages(&self, addr: VirtAddr, count: usize);
}
```

### Deallocation Result Types

Three distinct result types (matching the reference pattern):

```rust
/// Result from SlabAllocator::dealloc() (local path only).
pub enum SlabDeallocResult {
    /// Object freed, nothing else to do.
    Done,
    /// The slab page became empty and should be returned to buddy.
    FreeSlab { base: VirtAddr, pages: usize },
}

/// Result from SlabPoolTrait::dealloc() (routes local vs remote).
pub enum SlabPoolDeallocResult {
    /// Object freed on the local CPU path.
    Done,
    /// Object was queued onto the owner's remote-free list.
    RemoteQueued,
    /// The slab page became empty and should be returned to buddy.
    FreeSlab { base: VirtAddr, pages: usize },
}
```

### Public API

```rust
impl SlabAllocator {
    pub const fn new(page_size: usize) -> Self;
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult>;
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult;
    pub fn add_slab(&mut self, size_class: SizeClass, base: VirtAddr, bytes: usize, owner_cpu: u16);
}
```

### SMP-ready Design

- Per-CPU slab caches with `SpinNoIrq`
- Lock-free cross-CPU remote-free via atomic CAS on SlabPageHeader
- Owner CPU drains remote frees during subsequent local operations
- Initialized with 1 CPU; can be expanded for SMP later
- `current_cpu_id` provided via function pointer at pool creation

### Size Class Selection

- `layout.size().max(layout.align())` → select smallest fitting class
- Objects > 2048 bytes do not go through slab

## 3. ecraos::mem::alloc — Integration Layer

### File Structure

```
ecraos/src/mem/
├── alloc/
│   ├── mod.rs      # EcraosAllocator, GlobalAlloc, PageProvider impl, init_allocators()
│   └── test.rs     # Smoke tests, removable after verification
├── early.rs
├── reloc.rs
├── sections.rs
└── vmm.rs
```

### EcraosAllocator

```rust
struct EcraosAllocator {
    buddy: SpinNoIrq<BuddyAllocator>,
    slab_pool: &'static StaticSlabPool<1>,
}

impl PageProvider for EcraosAllocator {
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<VirtAddr> {
        let paddr = self.buddy.lock().alloc_frames(count, align)?;
        Ok(VirtAddr::from_usize(paddr.as_usize() + /* virt_phys_offset */))
    }
    fn dealloc_pages(&self, addr: VirtAddr, count: usize) {
        let paddr = PhysAddr::from_usize(addr.as_usize() - /* virt_phys_offset */);
        self.buddy.lock().dealloc_frames(paddr, count)
    }
}
```

### GlobalAlloc Implementation

```rust
unsafe impl GlobalAlloc for EcraosAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8;
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout);
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8;
}
```

Routing:

- Small objects (size <= 2048 && align <= 2048): slab path
- Large objects: buddy direct page allocation
- Realloc: allocate new + copy + free old

### Lock Ordering

To prevent deadlock, the slab allocation path follows this lock ordering (same as the reference implementation):

1. **Normal slab local path**: Only slab lock held
2. **Normal buddy page path**: Only buddy lock held
3. **Slab needs new page (NeedsSlab)**: Release slab lock → acquire buddy lock → release buddy lock → re-acquire slab lock → add slab → allocate
4. **Slab returns empty page (FreeSlab)**: Release slab lock → acquire buddy lock → release buddy lock

The `PageProvider::alloc_pages()` must never be called while holding the slab lock. The integration layer's GlobalAlloc implementation is responsible for this ordering.

### Deallocation Path

For small objects:

1. Determine the slab page header from the object's virtual address
2. Check `owner_cpu` in the slab page header
3. If local CPU: lock slab, call `dealloc_local()`, handle `FreeSlab` result by calling `PageProvider::dealloc_pages()`
4. If remote CPU: push to lock-free remote-free stack (no slab lock needed), return `RemoteQueued`

For large objects: convert virtual pointer to PhysAddr, call `buddy.dealloc_frames()`.

### Public Interface for Kernel Modules

```rust
pub fn alloc_frame() -> AllocResult<PhysAddr>;
pub fn alloc_frames(count: usize, align: usize) -> AllocResult<PhysAddr>;
pub unsafe fn alloc_frames_at(paddr: PhysAddr, count: usize) -> AllocResult<PhysAddr>;
pub fn dealloc_frames(addr: PhysAddr, count: usize);
pub fn dealloc_frame(addr: PhysAddr);
pub fn usage() -> AllocatorUsage;
```

### Initialization Flow

Called from `kernel_entry_with_vmm()` after `init_vmm_later()`:

1. Get boot memory regions (from `explat::mem::boot_mem_regions`)
2. Read `page_size` from `vmm::page_size_shift()` and `virt_phys_offset` from `vmm::virt_phys_offset()`
3. Create and init `BuddyAllocator` with page_size and virt_phys_offset
4. Register all usable regions, excluding:
   - Kernel image range (from `sections::kernel_range()`)
   - Early page allocator range (already returned from `destroy_early_page_allocator()`)
5. Create `StaticSlabPool` with 1 CPU
6. Declare `#[global_allocator]` static
7. Run smoke tests (in test module)

### #[global_allocator] Static

The `EcraosAllocator` is declared as a `static` with `#[global_allocator]` attribute. Tests using `Box`, `Vec`, `String` run after this is set up.

## 4. Workspace Changes

### New Cargo.toml entries

`exbuddy/Cargo.toml`:
- `#![no_std]` crate
- `memory_addr` dependency
- `kspin` dependency

`exslab/Cargo.toml`:
- `#![no_std]` crate
- `memory_addr` dependency
- `kspin` dependency

Workspace `Cargo.toml`:
- Add `exbuddy` and `exslab` to members

`ecraos/Cargo.toml`:
- Add `exbuddy` and `exslab` dependencies

## 5. Testing

### Test Module: `ecraos/src/mem/alloc/test.rs`

Encapsulated in a `test` submodule, called via `test::run()` from `kernel_entry_with_vmm`. Easily removable after verification.

### Buddy Allocator Tests

- `alloc_frame()` — check returned address is aligned and within a usable region
- `alloc_frames(4, page_size)` — check 4-page alignment
- `alloc_frames_at(specific_paddr, 1)` — check returned address matches
- `dealloc_frame()` / `dealloc_frames()` — free then reallocate, verify address reuse
- `usage()` — verify total_pages > 0, used_pages increases with allocation

### Slab Allocator Tests

- `Box::new(42u32)` — small object allocation via GlobalAlloc
- Allocate and free multiple times — no panic
- `Vec<u8>` with data push — verify correct write
- Large object allocation (>2048 bytes) — verify buddy path

### Integration Tests

- `String::from("hello ecraOS")` — full alloc/dealloc path
- Loop allocate/free 100 times — verify no leak (usage before/after consistent)

### Verification Method

All test output via `kprintln!` to QEMU serial. Build and run with `./test.sh`. Success = kernel reaches end of test output without panic or timeout.
