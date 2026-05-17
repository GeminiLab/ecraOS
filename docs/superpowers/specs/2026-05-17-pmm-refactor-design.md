# Physical Memory Management Refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor physical memory management to compute a unified, flag-annotated physical memory region table early in boot, and use it consistently across page table creation, early page allocation, and final allocator initialization.

**Architecture:** Replace ad-hoc region exclusion logic with a single `ecraos::mem::pmm` module that builds a final region table by splitting raw platform regions around the loader and kernel image. All downstream consumers (page tables, early allocator, buddy+slab) use this table exclusively. The early page allocator is destroyed after the final allocator is initialized, and its in-use pages are marked as allocated via `alloc_frame_at`.

**Tech Stack:** Rust `no_std`, `heapless::Vec`, `bitflags`, `memory_addr` types, existing `exbuddy`/`exslab` crates.

---

## 1. Boot Flow and BootArg Changes

### 1.1 Loader Boot Stack Fix (blocking)

The current uncommitted changes to move the boot stack from `.bss` to `.data` in the loader linker script cause a triple fault during kernel boot. This must be diagnosed and fixed before any other work proceeds.

The root cause: moving the 16 KiB boot stack into `.data` shifts `_skernel` (and therefore the kernel payload) by 16 KiB. The kernel enters successfully but crashes during early execution (relocation or BSS clearing phase).

**Requirements:**
- The boot stack must be in the `.data` section (not `.bss`) to avoid overlap with the kernel's BSS
- The loader binary must boot successfully to the kernel entry point in both debug and release modes
- The loader's BSS section must be documented as unreliable (overlaps with kernel BSS) and avoided; future technical means should prevent its use

**Files:** `ecraos-loader/link.ld`, `ecraos-loader/src/main.rs`

### 1.2 BootArg Field Rename

Rename `BootArg.boot_stack: PhysAddrRange` to `BootArg.loader_range: PhysAddrRange`.

The loader passes its entire memory range (`_sloader.._skernel`) instead of just the boot stack range. This covers the loader's `.text`, `.rodata`, `.data` (including boot stack), but excludes the kernel payload section.

**Changes:**
- `exboot/src/lib.rs`: Rename `boot_stack` field to `loader_range`
- `exboot-multiboot-x86_64/src/lib.rs`: `rust_entry64_bsp` constructs `BootArg { loader_range: PhysAddrRange::new_unchecked(_sloader, _skernel), ... }` using `_sloader` and `_skernel` symbols
- `ecraos/src/mem.rs`: Replace `BOOT_STACK_RANGE`/`boot_stack_range()` with `LOADER_RANGE`/`loader_range()`

---

## 2. MemoryRegion and Flags

### 2.1 Rename and Restructure

Rename `BootMemoryRegion` → `MemoryRegion`, `BootMemoryRegions` → `MemoryRegions`. `MAX_BOOT_MEM_REGIONS` name and value (48) remain unchanged.

Replace the `type_: BootMemoryRegionType` field with `flags: MemoryRegionFlags` using `bitflags`. Uncomment the existing `BootMemoryRegionFlags` code and rename to `MemoryRegionFlags`:

```rust
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryRegionFlags: usize {
        const READ         = 1 << 0;
        const WRITE        = 1 << 1;
        const EXECUTE      = 1 << 2;
        const DEVICE       = 1 << 4;
        const UNCACHED     = 1 << 5;
        const RESERVED     = 1 << 6;
        const FREE         = 1 << 7;
        const BOOT_SERVICE = 1 << 8;
    }
}
```

Reuse the commented-out constants updated for the new system:
- `DEFAULT_RAM_FLAGS`: `FREE | READ | WRITE`
- `DEFAULT_RESERVED_FLAGS`: `RESERVED | READ | WRITE`

### 2.2 Platform Population Rules

In `explat-x86_64::get_multiboot_memory_regions`:
- Multiboot `Available` with `base >= 1 MiB` → `DEFAULT_RAM_FLAGS`
- Multiboot `Available` with `base < 1 MiB` → `DEFAULT_RESERVED_FLAGS`
- Multiboot `Reserved` / `ACPI` / `NVS` → `DEFAULT_RESERVED_FLAGS`
- `Defect` → skip

`BOOT_SERVICE` is NOT set by the platform layer. The kernel applies it when splitting out the loader range.

**Files:** `explat/explat/src/mem.rs`, `explat/explat-x86_64/src/mem.rs`

---

## 3. Physical Memory Region Table (`ecraos::mem::pmm`)

### 3.1 Data Structure

```rust
pub type PhysMemRegions = HeaplessVec<(MemoryRegion, &'static str), 128>;
```

128 entries provide ample headroom (raw ~7 multiboot regions → ~20-30 after splitting).

### 3.2 Build Function

```rust
pub fn build_phys_mem_regions(
    boot_regions: &MemoryRegions,
    loader_range: PhysAddrRange,
) -> PhysMemRegions
```

Located in `ecraos::mem::pmm` (a new module). Since this is inside `ecraos`, it has direct access to `sections::all_sections()` — no `SectionInfo` abstraction needed.

**Algorithm:**
1. Start with all `boot_regions`
2. For each region containing `FREE`, split it around:
   - `loader_range` → `BOOT_SERVICE | READ | WRITE`, label "loader"
   - Kernel `.text` aligned range → `READ | EXECUTE`, label "kernel .text"
   - Kernel `.rodata` aligned range → `READ`, label "kernel .rodata"
   - Kernel `.data` / `.rela_dyn` / `.got` aligned ranges → `READ | WRITE`, label "kernel .data"
   - Kernel `.bss` aligned range → `READ | WRITE`, label "kernel .bss"
   - Gaps from splitting retain `FREE | READ | WRITE`, label "free"
3. Non-`FREE` regions (reserved) keep their flags, label "reserved"

The splitting algorithm is iterative: for each excluded range, split every existing sub-region into non-overlapping pieces, inheriting flags from the parent (FREE) or applying specific flags (loader/kernel sections).

### 3.3 Static Storage

The result is stored in a `MaybeUninit<PhysMemRegions>` static in `ecraos::mem::pmm`, initialized once during `init_vmm`. A `saved_phys_mem_regions()` accessor returns `&'static PhysMemRegions`.

### 3.4 Invocation

Called in `init_vmm` after getting boot regions and saving the loader range, before any page table or allocator setup. After this call, the raw `boot_regions` is never used again.

**Printing:** In `kernel_entry`, after calling `build_phys_mem_regions`, print the full table with labels for debugging.

**Files:** New `ecraos/src/mem/pmm.rs`, modified `ecraos/src/mem.rs`, `ecraos/src/main.rs`

---

## 4. Page Table Mapping

### 4.1 Mapping from Region Flags

In `init_vmm`, when creating the early page table, iterate over the final `PhysMemRegions` table instead of the raw boot regions.

Map `MemoryRegionFlags` to `MappingFlags`:
- `READ` present → `MappingFlags::READ`
- `WRITE` present → `MappingFlags::WRITE`
- `EXECUTE` present → `MappingFlags::EXECUTE`
- No `WRITE` → read-only mapping
- No `EXECUTE` → no-execute (NX bit if supported)

Both identity mapping (`vaddr_low`) and high-half mapping (`vaddr_high`) are created for each region. The boot-stage 1 GiB huge-page mapping is unaffected — this only applies to the fine-grained page table created during VMM setup.

**Files:** `ecraos/src/mem.rs` (in `init_vmm`)

---

## 5. Early Page Allocator

`find_early_page_allocator_range` takes `&PhysMemRegions` instead of `&MemoryRegions`. It searches `FREE | READ | WRITE` regions for 512 pages of contiguous space at the end of a region, avoiding overlap with any non-FREE region (which now correctly includes kernel and loader ranges).

**Files:** `ecraos/src/mem/early.rs`

---

## 6. Final Allocator Initialization

### 6.1 Sequence

The initialization sequence in `init_allocators` is restructured:

1. **Destroy early page allocator** → get `(bitmap, page_size_shift, base_paddr)`
2. **Compute early allocator range:** `base_paddr..base_paddr + 512 * (1 << page_size_shift)`
3. **Iterate `FREE` regions** from the final region table:
   - For each FREE region, check if the early allocator range falls within it
   - **Overlap check (point 9):** Use `BuddySection::compute_region_layout` to compute where buddy metadata would be placed. If the metadata range `[section_start, managed_heap_start)` overlaps the early allocator range → panic. Otherwise, add the entire region to the buddy allocator.
   - If the early allocator range is NOT in this region, add the region directly.
4. **Mark early allocator's in-use pages:** For each set bit in the bitmap, compute the physical address and call `alloc_frame_at` on the buddy allocator.
5. **Initialize slab pool** and set `INITIALIZED`.

### 6.2 Buddy Metadata Overlap Check

The `exbuddy::BuddyAllocator` uses intrusive metadata — `BuddySection` header + `PageMeta` array are stored at the start of each managed region. `BuddySection::compute_region_layout` returns a `RegionLayout` struct with `section_start`, `meta_start`, `managed_heap_start`, and `managed_heap_size`.

The check: if `managed_heap_start > early_allocator_start` AND `section_start < early_allocator_end`, the metadata overlaps the early allocator → panic.

### 6.3 Code Removal

Remove from `ecraos/src/mem/alloc/mod.rs`:
- The entire region-carving algorithm (the `sub_regions` array loop with `excluded` ranges)
- Direct use of `mem::boot_stack_range()`, `mem::early::early_allocator_range()`, kernel range computation

Remove from `ecraos/src/mem.rs`:
- `SAVED_MEM_REGIONS` / `saved_mem_regions()` — no longer needed
- `BOOT_STACK_RANGE` / `boot_stack_range()` — replaced by `LOADER_RANGE` / `loader_range()`

Remove from `ecraos/src/mem/early.rs`:
- `EARLY_ALLOCATOR_RANGE` / `early_allocator_range()` — range determined directly from destroy result

The raw `boot_mem_regions` result is used once in `init_vmm` (to build the final table) and never again.

---

## 7. Summary of File Changes

| File | Action |
|------|--------|
| `ecraos-loader/link.ld` | Fix boot stack placement |
| `ecraos-loader/src/main.rs` | Document BSS unreliability |
| `exboot/exboot/src/lib.rs` | Rename `boot_stack` → `loader_range` |
| `exboot-multiboot-x86_64/src/lib.rs` | Pass `_sloader.._skernel` as `loader_range` |
| `explat/explat/src/mem.rs` | `BootMemoryRegion` → `MemoryRegion`, `BootMemoryRegionType` → `MemoryRegionFlags` |
| `explat/explat-x86_64/src/mem.rs` | Populate flags instead of type enum |
| `ecraos/src/mem/pmm.rs` | **New**: `build_phys_mem_regions`, static storage |
| `ecraos/src/mem.rs` | Call `build_phys_mem_regions`, use final table for page tables, replace `BOOT_STACK_RANGE` with `LOADER_RANGE` |
| `ecraos/src/mem/early.rs` | Use `PhysMemRegions`, remove `EARLY_ALLOCATOR_RANGE` |
| `ecraos/src/mem/alloc/mod.rs` | Use `PhysMemRegions` FREE regions, remove region-carving, restructure init sequence |
| `ecraos/src/main.rs` | Print phys mem regions in `kernel_entry` |

---

## 8. Task Ordering and Dependencies

1. **Fix loader boot stack** (blocking) — must pass debug+release boot before anything else
2. **Rename BootArg, MemoryRegion, MemoryRegionFlags** — foundation for all subsequent changes
3. **Implement `ecraos::mem::pmm`** — the core computation
4. **Wire pmm into init_vmm** — page tables use final region table
5. **Wire pmm into early allocator** — uses final region table
6. **Wire pmm into final allocator** — uses final region table, new init sequence
7. **Remove dead code** — clean up old statics and region-carving logic
8. **Test** — debug + release mode boot with allocator smoke tests
