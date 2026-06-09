# expt Page Table Cursor Design

**Goal:** Move every page-table mutating operation in `expt` behind a
`PageTableCursor`, track affected virtual mappings while the cursor is alive,
and automatically flush the TLB once when the cursor is dropped.

**Scope:** This design covers the `expt` crate and the existing `ecraos`
call sites that build early page tables. It does not introduce address-space
management, page-table deallocation policy changes, ASIDs/PCIDs, SMP shootdown,
or transactional rollback for failed range operations.

**Reference:** `refs/page_table_multiarch` is used only as a behavioral and API
reference. The implementation in `expt` should follow local generic multi-level
page-table structure rather than copying reference source.

---

## 1. Current Context

`expt` currently has a small public surface:

- `PageTable<M, PTE>` owns a root physical address.
- `PageTableMeta` describes the page-table shape.
- `PagingHandler` provides page-table page allocation and physical-to-virtual
  translation.
- `PageTable::map::<H>()` and `PageTable::unmap::<H>()` are public mutating
  entry points.
- Internal mutating helpers include `get_page_entry_mut`, `next_table_mut`,
  `clear_pte`, and `iter_pages_in_range`.

The only current project call site is early VMM setup in `ecraos/src/mem.rs`,
where physical memory regions are mapped into a fresh page table before loading
`cr3`.

---

## 2. Chosen API Shape

Use the conservative API change:

```rust
impl<M: PageTableMeta, PTE: GenericPTE> PageTable<M, PTE> {
    pub fn cursor<H: PagingHandler>(&mut self) -> PageTableCursor<'_, M, PTE, H>;
}
```

`PageTable<M, PTE>` keeps its current type shape. `PagingHandler` remains a
method-level type parameter instead of becoming part of `PageTable`.

`PageTableCursor<'a, M, PTE, H>` holds `&'a mut PageTable<M, PTE>` and owns all
operations that can modify PTEs or whose correctness depends on page-table
modification side effects:

- public `map`
- public `unmap`
- private mutating traversal helpers
- private table creation
- private huge-page splitting
- private recursive clearing
- flush tracking

`PageTable::map` and `PageTable::unmap` stop being public modification entry
points. Call sites must explicitly create a cursor.

The early VMM setup should create one cursor around the whole physical-region
mapping loop rather than creating one cursor per region. This preserves batch
flush behavior:

```rust
let mut cursor = early_page_table.cursor::<early::EarlyPagingHandler>();
for region in pmm::phys_mem_regions() {
    cursor.map(vaddr_low, paddr, size, mapping_flags)?;
    cursor.map(vaddr_high, paddr, size, mapping_flags)?;
}
drop(cursor);
```

In early boot this flush is usually redundant because the new page table has
not yet been loaded into `cr3`, but keeping the same cursor semantics avoids a
special boot-only path.

---

## 3. Platform Flush Interface

Add TLB flushing to `PageTableMeta`:

```rust
pub trait PageTableMeta: Send + Sync {
    type VirtAddr: MemoryAddr + Add<usize, Output = Self::VirtAddr> + LowerHex;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>);
}
```

Semantics:

- `Some(vaddr)` flushes the TLB entry for the mapping containing `vaddr`.
- `None` flushes the whole local TLB.

`X86Level4PageTableMeta` and `X86Level5PageTableMeta` implement this with the
x86_64 local TLB instructions. Future RISC-V metadata should implement the same
trait method with `sfence.vma`.

This design deliberately keeps the interface local-CPU only. Cross-core TLB
shootdown, ASID handling, and PCID handling are out of scope.

---

## 4. Cursor Flush Tracking

`expt` depends on `heapless` and uses fixed-capacity storage for flush records:

```rust
const SMALL_FLUSH_THRESHOLD: usize = 32;

enum TlbFlusher<M: PageTableMeta> {
    None,
    Pages(heapless::Vec<M::VirtAddr, SMALL_FLUSH_THRESHOLD>),
    Full,
}
```

The cursor exposes two internal recording methods:

```rust
fn push_flush_addr(&mut self, vaddr: M::VirtAddr);
fn push_flush_all(&mut self);
```

`push_flush_addr` records one affected virtual mapping. If the fixed vector is
full, the cursor switches to `Full`. In `Full` state, later address pushes are
no-ops.

`push_flush_all` directly switches the cursor to `Full`. It is used for
structural changes where single-address flushing is too easy to get wrong.

The cursor provides:

```rust
pub fn flush(&mut self);
```

`flush()` calls `M::flush_tlb(Some(vaddr))` for recorded addresses, or
`M::flush_tlb(None)` for `Full`, then resets the state to `None`. It returns no
`Result`. The cursor `Drop` implementation calls `flush()`, so manual flush is
idempotent with drop.

---

## 5. Huge Page Rules

Huge pages need conservative handling.

When traversal reaches a non-empty huge-page leaf and needs to split it into a
lower-level table, the cursor must call `push_flush_all()` directly. The code
should include a short comment explaining why: the old leaf PTE may have
created a hardware TLB entry at huge-page granularity, and replacing it with a
table PTE cannot safely be represented as a small number of ordinary page
flushes.

After `push_flush_all()`:

- replacing the huge-page PTE with a table PTE needs no additional per-page
  flush record;
- filling the newly created lower-level table needs no additional per-page
  flush record;
- later changes to the target small page may still call `push_flush_addr`, but
  that is a no-op while the cursor is already in `Full` state.

Recursive clear follows the same principle:

- clearing a normal leaf page records that leaf's virtual address;
- clearing a huge leaf calls `push_flush_all()`;
- clearing a table recursively lets children determine whether they need a
  page flush or a full flush.

This prioritizes correctness and clear teaching value over micro-optimized huge
page invalidation.

---

## 6. Map and Unmap Semantics

Cursor `map` keeps the current range-oriented behavior:

- the input range is aligned down/up to the base page size;
- the implementation chooses the largest valid page level for each chunk;
- current mappings in the covered range are cleared first;
- `vaddr - paddr` offset is preserved across the mapped range;
- existing mappings are overwritten rather than treated as `AlreadyMapped`.

Cursor `unmap` also keeps the current range behavior:

- the input range is aligned down/up to the base page size;
- huge mappings are split or cleared as needed;
- present leaves in the range are removed;
- the operation is not transactional.

The existing `PagingError` variants remain unchanged:

- `NotMapped`
- `AlreadyMapped`
- `MappedToHugePage`
- `AllocationFailed`
- `CannotBePage { level }`

The cursor must record flushes immediately after each successful PTE change
that affects a mapping. If a range operation later returns an error, already
completed changes still flush on cursor drop.

No flush is recorded for an error path that did not change any PTE affecting a
mapping, such as an invalid level or an allocation failure before replacing an
entry.

---

## 7. Internal Refactor Boundaries

The implementation should keep edits focused in `expt`:

- Add `PageTableCursor` and `TlbFlusher` in `expt/src/lib.rs`, unless the file
  becomes too large during implementation.
- Keep `PageTable` responsible for root ownership, creation, `base_paddr`, and
  read-only helpers.
- Move or adapt mutating helpers so they are called through `PageTableCursor`.
- Keep non-allocating table access helpers close to their current form.
- Add `heapless = "0.9"` to `expt/Cargo.toml`.

The migration in `ecraos/src/mem.rs` should be minimal: create one cursor for
the early mapping loop and call cursor methods.

---

## 8. Test Harness

Add comprehensive crate-local tests under `#[cfg(test)]`.

The tests use local fake types:

- `TestMeta`: a small multi-level page-table shape, for example three levels
  with `LEVEL_BITS = [2, 2, 2]`, 4 KiB base pages, and small huge-page levels.
- `TestPte`: a simple `GenericPTE` implementation that explicitly represents
  unused, table, normal page, and huge page entries.
- `TestPagingHandler`: host allocation for page-table pages, tracking
  allocated tables and supporting controlled allocation failure.
- flush log: `TestMeta::flush_tlb` records `Some(vaddr)` and `None` in
  thread-local state instead of executing hardware instructions.

Tests should include helpers to walk the page table from root and query a
virtual address's effective mapping. These helpers are test-only or private
crate helpers and do not become public API.

---

## 9. Functional Test Coverage

The tests should cover page-table behavior, not only TLB tracking:

1. `new_alloc`
   - root table allocation succeeds;
   - root table is zeroed;
   - `base_paddr()` returns the allocated root;
   - allocation failure returns `AllocationFailed`.

2. Single-page map and unmap
   - map a base page and query the expected physical address and flags;
   - unmap it and verify it is no longer mapped;
   - unmapping an already empty range returns `Ok(())` if any table creation
     needed by traversal succeeds, leaves no present mapping behind, and records
     no TLB flush for a present mapping that did not exist.

3. Range map
   - aligned and unaligned ranges are normalized to base-page boundaries;
   - virtual-to-physical offset is preserved;
   - every covered base page resolves to the expected physical address.

4. Page-size selection
   - aligned ranges use the largest valid page level;
   - mixed ranges split into the expected huge and base-page leaves;
   - non-aligned ranges fall back to smaller leaves where necessary.

5. Overwrite/remap behavior
   - mapping over an existing range replaces old mappings;
   - new physical addresses and flags are visible;
   - old lower-level table mappings do not remain effective after huge-page
     overwrite.

6. Huge-page split
   - map a huge page;
   - modify or unmap one base page inside it;
   - verify the target page changed;
   - verify unaffected subpages retain their old physical mapping and flags.

7. Table-to-huge overwrite
   - map multiple base pages;
   - map a huge page over the same range;
   - verify lookup resolves through the new huge leaf.

8. Partial failure
   - force allocation failure after part of a range succeeds;
   - assert the returned error;
   - assert already completed mappings remain visible;
   - assert later uncompleted mappings are not falsely installed.

9. Small pressure test
   - run a fixed-seed deterministic sequence of map/unmap operations over a
     small virtual address space;
   - maintain a base-page shadow model;
   - compare effective mappings after each operation or after dense
     checkpoints;
   - include overlap, repeated, boundary, and huge-page-crossing operations.

---

## 10. Cursor and TLB Test Coverage

The cursor-specific tests should cover:

1. Drop auto flush
   - perform a map through a cursor;
   - let the cursor go out of scope;
   - assert a single address flush was recorded.

2. Manual flush idempotence
   - call `flush()` explicitly;
   - assert the flush happened;
   - drop the cursor;
   - assert no duplicate flush happened.

3. Threshold fallback
   - modify 33 distinct mappings;
   - assert the cursor emits a full flush rather than 33 address flushes.

4. Cursor-only mutation
   - tests and production call sites use `cursor::<H>()`;
   - `PageTable::map` and `PageTable::unmap` are no longer public methods.

5. Huge-page full flush
   - splitting a huge page records full flush;
   - clearing a huge leaf records full flush.

6. Partial success flush
   - perform a range operation that modifies at least one mapping and later
     errors;
   - assert the modified portion still flushes when the cursor drops.

7. Read-only operations do not flush
   - test-only lookup and any retained read-only methods must not record flush
     requests.

---

## 11. Verification

Implementation should be verified with:

```sh
cargo test -p expt
cargo check -p ecraos
```

If workspace or kernel-target checks are blocked by target configuration, the
implementation result should report the exact command and failure reason.

Completion requires:

- all page-table modifications are reachable only through `PageTableCursor`;
- Cursor drop reliably flushes accumulated changes;
- huge-page split and huge-leaf clear use full flush;
- `heapless::Vec` with threshold 32 is used for small flush tracking;
- existing early VMM call sites are migrated to cursor usage;
- the comprehensive `expt` tests pass.
