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

## 1. exbuddy — Buddy Page Allocator

### Data Structures

**`BuddySection`** — per-region descriptor stored in the region prefix:

- `next: *mut BuddySection` — intrusive linked list pointer
- `region_start: PhysAddr`, `region_size: usize`
- `meta: *mut PageMeta` — pointer to PageMeta array
- `max_pages: usize`
- `heap_start: PhysAddr`, `heap_size: usize`
- `free_lists: [u32; MAX_ORDER + 1]` — intrusive free list heads per order
- `free_pages: usize`, `total_pages: usize`

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

**`AllocatorUsage`**: `{ total_pages: usize, used_pages: usize }`

### Initialization

Metadata (BuddySection + PageMeta[]) is placed in the region prefix. The allocator scans the heap portion from low to high, splitting into maximally-aligned buddy blocks.

`init()` receives a `PhysAddrRange`, `page_size`, and `virt_phys_offset`. Additional regions added via `add_region()`.

### Public API

```rust
impl BuddyAllocator {
    pub const fn new() -> Self;

    /// Initialize over the first region. Stores page_size and virt_phys_offset.
    pub unsafe fn init(
        &mut self,
        region: PhysAddrRange,
        page_size: usize,
        virt_phys_offset: usize,
    ) -> AllocResult;

    /// Add another usable region after init.
    pub unsafe fn add_region(&mut self, region: PhysAddrRange) -> AllocResult;

    /// Allocate `count` contiguous pages with `align` alignment.
    pub fn alloc_frames(&mut self, count: usize, align: usize) -> AllocResult<PhysAddr>;

    /// Allocate a single page.
    pub fn alloc_frame(&mut self) -> AllocResult<PhysAddr>;

    /// Allocate `count` pages at a specific physical address.
    pub unsafe fn alloc_frames_at(&mut self, paddr: PhysAddr, count: usize) -> AllocResult<PhysAddr>;

    /// Deallocate `count` contiguous pages.
    pub fn dealloc_frames(&mut self, addr: PhysAddr, count: usize);

    /// Deallocate a single page.
    pub fn dealloc_frame(&mut self, addr: PhysAddr);

    /// Set page flags (used by slab to tag pages).
    pub fn set_page_flags(&mut self, addr: PhysAddr, flags: PageFlags) -> AllocResult;

    /// Return usage statistics.
    pub fn usage(&self) -> AllocatorUsage;
}
```

### alloc_frames_at Implementation

1. Find the section containing `paddr`
2. Compute the PFN of the first page
3. Check all requested pages are free
4. Remove them from free lists (may require splitting and re-splitting blocks)
5. Mark as Allocated

### Internal phys/virt Conversion

```rust
fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
    VirtAddr::from_usize(paddr.as_usize() + self.virt_phys_offset)
}

fn virt_to_phys(&self, vaddr: VirtAddr) -> PhysAddr {
    PhysAddr::from_usize(vaddr.as_usize() - self.virt_phys_offset)
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

### PageProvider Trait

```rust
/// Trait for slab to request/return pages from a backend allocator.
/// Implemented by the integration layer in ecraos.
pub trait PageProvider: Sync {
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<PhysAddr>;
    fn dealloc_pages(&self, addr: PhysAddr, count: usize);
}
```

### Public API

```rust
impl SlabAllocator {
    pub const fn new(page_size: usize) -> Self;
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult>;
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult;
    pub fn add_slab(&mut self, size_class: SizeClass, base: PhysAddr, bytes: usize, owner_cpu: u16);
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
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<PhysAddr> {
        self.buddy.lock().alloc_frames(count, align)
    }
    fn dealloc_pages(&self, addr: PhysAddr, count: usize) {
        self.buddy.lock().dealloc_frames(addr, count)
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

1. Get boot memory regions
2. Create and init BuddyAllocator with page_size and virt_phys_offset
3. Register all usable regions (excluding kernel-occupied areas)
4. Create StaticSlabPool with 1 CPU
5. Set global allocator singleton
6. Run smoke tests (in test module)

## 4. Workspace Changes

### New Cargo.toml entries

`exbuddy/Cargo.toml`:
- `memory_addr` dependency
- `kspin` dependency

`exslab/Cargo.toml`:
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
