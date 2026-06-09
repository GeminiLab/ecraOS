# expt Cursor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `PageTableCursor` to `expt` so all page-table mutations go through the cursor, affected virtual mappings are tracked, and the cursor flushes the TLB on `flush()` or `Drop`.

**Architecture:** Keep `PageTable<M, PTE>` as the owner of the root page table and move mutating traversal, `map`, `unmap`, huge-page split, recursive clear, and flush tracking into `PageTableCursor<'a, M, PTE, H>`. Add a `PageTableMeta::flush_tlb` platform hook and a bounded `heapless::Vec`-backed flusher that falls back to full flush at 32 addresses. Tests use a local fake PTE/meta/handler harness so mapping semantics and TLB behavior can be verified on the host without executing hardware TLB instructions.

**Tech Stack:** Rust 2024, `no_std` crate with `std` available under `#[cfg(test)]`, `page_table_entry::GenericPTE`, `memory_addr`, `heapless::Vec`, `cargo test -p expt`, `cargo check -p ecraos`.

---

## File Structure

- Modify `expt/Cargo.toml`
  - Add `heapless = "0.9"` to runtime dependencies.

- Modify `expt/src/meta.rs`
  - Add `fn flush_tlb(vaddr: Option<Self::VirtAddr>);` to `PageTableMeta`.
  - Keep all layout constants in this file.

- Modify `expt/src/lib.rs`
  - Add `#[cfg(test)] extern crate std;` near the crate attributes.
  - Implement `PageTable::cursor::<H>()`.
  - Add `SMALL_FLUSH_THRESHOLD`, `TlbFlusher`, and `PageTableCursor`.
  - Keep `PageTable` owning root allocation and non-mutating/static helpers.
  - Move mutating helpers from `PageTable` to `PageTableCursor`.
  - Remove public `PageTable::map` and `PageTable::unmap` as mutation entry points.
  - Add a `#[cfg(test)] mod tests` harness and comprehensive tests.

- Modify `ecraos/src/mem.rs`
  - Replace direct `early_page_table.map::<early::EarlyPagingHandler>(...)` calls with one cursor created before the physical-region loop.

- No new public modules are required unless `expt/src/lib.rs` becomes too large during implementation. If splitting becomes necessary, create only `expt/src/cursor.rs` for cursor-specific code and re-export `PageTableCursor` from `lib.rs`.

---

### Task 1: Add The Test Harness Types

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add `std` support for tests**

Insert this after the crate feature attributes in `expt/src/lib.rs`:

```rust
#[cfg(test)]
extern crate std;
```

- [ ] **Step 2: Add the test harness module**

Append this module to the end of `expt/src/lib.rs`. This module should compile before cursor tests are added.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::Layout;
    use core::cell::RefCell;
    use memory_addr::{MemoryAddr, PhysAddr, VirtAddr};
    use page_table_entry::{GenericPTE, MappingFlags};
    use std::alloc::{alloc, dealloc};
    use std::vec::Vec;

    const PTE_UNUSED: u8 = 0;
    const PTE_TABLE: u8 = 1;
    const PTE_PAGE: u8 = 2;
    const PTE_HUGE: u8 = 3;

    #[derive(Clone, Copy, Debug)]
    struct TestPte {
        paddr: usize,
        flags: MappingFlags,
        kind: u8,
        _reserved: [u8; 7],
    }

    impl TestPte {
        fn empty() -> Self {
            Self {
                paddr: 0,
                flags: MappingFlags::empty(),
                kind: PTE_UNUSED,
                _reserved: [0; 7],
            }
        }
    }

    impl Default for TestPte {
        fn default() -> Self {
            Self::empty()
        }
    }

    impl GenericPTE for TestPte {
        fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
            Self {
                paddr: paddr.as_usize(),
                flags,
                kind: if is_huge { PTE_HUGE } else { PTE_PAGE },
                _reserved: [0; 7],
            }
        }

        fn new_table(paddr: PhysAddr) -> Self {
            Self {
                paddr: paddr.as_usize(),
                flags: MappingFlags::empty(),
                kind: PTE_TABLE,
                _reserved: [0; 7],
            }
        }

        fn paddr(&self) -> PhysAddr {
            PhysAddr::from_usize(self.paddr)
        }

        fn flags(&self) -> MappingFlags {
            self.flags
        }

        fn set_paddr(&mut self, paddr: PhysAddr) {
            self.paddr = paddr.as_usize();
        }

        fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
            self.flags = flags;
            self.kind = if is_huge { PTE_HUGE } else { PTE_PAGE };
        }

        fn bits(self) -> usize {
            self.paddr | self.kind as usize
        }

        fn is_unused(&self) -> bool {
            self.kind == PTE_UNUSED
        }

        fn is_present(&self) -> bool {
            matches!(self.kind, PTE_TABLE | PTE_PAGE | PTE_HUGE)
        }

        fn is_huge(&self) -> bool {
            self.kind == PTE_HUGE
        }

        fn clear(&mut self) {
            *self = Self::empty();
        }
    }

    struct TestMeta;

    impl PageTableMeta for TestMeta {
        type VirtAddr = VirtAddr;

        const LEVELS: usize = 3;
        const PAGE_OFFSET_BITS: usize = 12;
        const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
        const MAX_PAGE_LEVEL: usize = 2;

        fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
            FLUSH_LOG.with(|log| {
                log.borrow_mut().push(vaddr.map(|vaddr| vaddr.as_usize()));
            });
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct MappingSnapshot {
        paddr: usize,
        flags: MappingFlags,
        page_size: usize,
    }

    #[derive(Clone, Copy)]
    struct AllocationRecord {
        ptr: usize,
        layout: Layout,
    }

    #[derive(Default)]
    struct AllocState {
        allocations: Vec<AllocationRecord>,
        alloc_count: usize,
        fail_on_alloc: Option<usize>,
    }

    std::thread_local! {
        static ALLOC_STATE: RefCell<AllocState> = RefCell::new(AllocState::default());
        static FLUSH_LOG: RefCell<Vec<Option<usize>>> = RefCell::new(Vec::new());
    }

    struct TestPagingHandler;

    impl PagingHandler for TestPagingHandler {
        fn alloc_page_aligned(bytes_required: usize) -> Option<PhysAddr> {
            ALLOC_STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.alloc_count += 1;
                if state.fail_on_alloc == Some(state.alloc_count) {
                    return None;
                }

                let layout = Layout::from_size_align(bytes_required, 4096).ok()?;
                let ptr = unsafe { alloc(layout) };
                if ptr.is_null() {
                    return None;
                }

                state.allocations.push(AllocationRecord {
                    ptr: ptr as usize,
                    layout,
                });
                Some(PhysAddr::from_usize(ptr as usize))
            })
        }

        fn dealloc_page_aligned(addr: PhysAddr, bytes_deallocated: usize) {
            ALLOC_STATE.with(|state| {
                let mut state = state.borrow_mut();
                let index = state
                    .allocations
                    .iter()
                    .position(|record| record.ptr == addr.as_usize())
                    .expect("deallocating an unknown test page-table allocation");
                let record = state.allocations.remove(index);
                assert_eq!(record.layout.size(), bytes_deallocated);
                unsafe { dealloc(record.ptr as *mut u8, record.layout) };
            });
        }

        fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
            VirtAddr::from_usize(addr.as_usize())
        }
    }

    fn reset_test_state() {
        ALLOC_STATE.with(|state| {
            let mut state = state.borrow_mut();
            for record in state.allocations.drain(..) {
                unsafe { dealloc(record.ptr as *mut u8, record.layout) };
            }
            state.alloc_count = 0;
            state.fail_on_alloc = None;
        });
        FLUSH_LOG.with(|log| log.borrow_mut().clear());
    }

    fn fail_on_alloc(n: usize) {
        ALLOC_STATE.with(|state| state.borrow_mut().fail_on_alloc = Some(n));
    }

    fn flush_log() -> Vec<Option<usize>> {
        FLUSH_LOG.with(|log| log.borrow().clone())
    }

    fn clear_flush_log() {
        FLUSH_LOG.with(|log| log.borrow_mut().clear());
    }

    fn index_for(level: usize, vaddr: usize) -> usize {
        let (start, end) = TestMeta::LEVEL_BIT_RANGES[level];
        (vaddr >> start) & ((1 << (end - start)) - 1)
    }

    fn table_slice(paddr: PhysAddr, level: usize) -> &'static [TestPte] {
        let entry_count = TestMeta::LEVEL_TABLE_SIZE[level];
        let ptr = TestPagingHandler::phys_to_virt(paddr).as_ptr() as *const TestPte;
        unsafe { core::slice::from_raw_parts(ptr, entry_count) }
    }

    fn lookup(
        table: &PageTable<TestMeta, TestPte>,
        vaddr: usize,
    ) -> Option<MappingSnapshot> {
        let mut table_paddr = table.base_paddr();
        let mut level = TestMeta::LEVELS - 1;

        loop {
            let entries = table_slice(table_paddr, level);
            let entry = entries[index_for(level, vaddr)];
            if entry.is_unused() {
                return None;
            }
            if level == 0 || entry.is_huge() {
                let page_size = TestMeta::LEVEL_PAGE_SIZE[level];
                return Some(MappingSnapshot {
                    paddr: entry.paddr().as_usize() + vaddr % page_size,
                    flags: entry.flags(),
                    page_size,
                });
            }
            table_paddr = entry.paddr();
            level -= 1;
        }
    }

    fn new_table() -> PageTable<TestMeta, TestPte> {
        PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>().unwrap()
    }
}
```

- [ ] **Step 3: Run the harness compile check**

Run:

```bash
cargo test -p expt
```

Expected: FAIL because `TestMeta` implements `flush_tlb`, but `PageTableMeta` does not define that method yet. The relevant compiler error should mention that `flush_tlb` is not a member of trait `PageTableMeta`.

- [ ] **Step 4: Leave the failing harness uncommitted**

Do not commit this failing state. Continue to Task 2 to add the trait hook and make the harness compile.

---

### Task 2: Add `PageTableMeta::flush_tlb` And `heapless`

**Files:**
- Modify: `expt/Cargo.toml`
- Modify: `expt/src/meta.rs`
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add the dependency**

In `expt/Cargo.toml`, add `heapless` under `[dependencies]`:

```toml
heapless = "0.9"
```

- [ ] **Step 2: Add the metadata hook**

In `expt/src/meta.rs`, add this required method to `PageTableMeta` after `type VirtAddr`:

```rust
/// Flushes the local TLB.
///
/// `Some(vaddr)` flushes the entry for the mapping containing `vaddr`.
/// `None` flushes the whole local TLB.
fn flush_tlb(vaddr: Option<Self::VirtAddr>);
```

- [ ] **Step 3: Implement x86_64 TLB flushing for existing metadata types**

In `expt/src/lib.rs`, inside both `impl PageTableMeta for X86Level4PageTableMeta` and `impl PageTableMeta for X86Level5PageTableMeta`, add the same method:

```rust
fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if let Some(vaddr) = vaddr {
            let addr: usize = vaddr.into();
            core::arch::asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
        } else {
            let cr3: usize;
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
            core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = vaddr;
        unimplemented!("x86 page table metadata can only flush TLB on x86_64");
    }
}
```

- [ ] **Step 4: Run the tests**

Run:

```bash
cargo test -p expt
```

Expected: PASS. There are still no behavioral tests, but the harness and trait hook compile.

- [ ] **Step 5: Commit**

```bash
git add expt/Cargo.toml expt/src/meta.rs expt/src/lib.rs Cargo.lock
git commit -m "feat(expt): add page table TLB flush hook"
```

If `Cargo.lock` does not change because `heapless` is already locked by other crates, omit `Cargo.lock` from `git add`.

---

### Task 3: Add Failing Cursor/TLB Tests

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add Cursor drop and manual flush tests**

Inside `#[cfg(test)] mod tests`, append these tests:

```rust
#[test]
fn cursor_drop_flushes_single_page() {
    reset_test_state();
    let mut table = new_table();
    let vaddr = VirtAddr::from_usize(0x1000);
    let paddr = PhysAddr::from_usize(0x8000);

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                vaddr,
                paddr,
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
        assert!(flush_log().is_empty());
    }

    assert_eq!(flush_log(), std::vec![Some(0x1000)]);
    assert_eq!(
        lookup(&table, 0x1000),
        Some(MappingSnapshot {
            paddr: 0x8000,
            flags: MappingFlags::READ | MappingFlags::WRITE,
            page_size: TestMeta::PAGE_SIZE,
        })
    );
    reset_test_state();
}

#[test]
fn cursor_manual_flush_is_idempotent_with_drop() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x2000),
                PhysAddr::from_usize(0x9000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        assert_eq!(flush_log(), std::vec![Some(0x2000)]);
    }

    assert_eq!(flush_log(), std::vec![Some(0x2000)]);
    reset_test_state();
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run:

```bash
cargo test -p expt cursor_drop_flushes_single_page cursor_manual_flush_is_idempotent_with_drop
```

Expected: FAIL to compile with an error that `PageTable<TestMeta, TestPte>` has no method named `cursor`.

---

### Task 4: Implement The Basic Cursor Shell And Move `map`/`unmap`

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add cursor imports and flusher types**

Near the existing imports in `expt/src/lib.rs`, replace the `PhantomData` import with this:

```rust
use core::marker::PhantomData;

use heapless::Vec as HeaplessVec;
```

Then add these items above `pub struct PageTable`:

```rust
const SMALL_FLUSH_THRESHOLD: usize = 32;

enum FlushRequest<M: PageTableMeta> {
    None,
    Addr(M::VirtAddr),
    All,
}

enum TlbFlusher<M: PageTableMeta> {
    None,
    Pages(HeaplessVec<M::VirtAddr, SMALL_FLUSH_THRESHOLD>),
    Full,
}

pub struct PageTableCursor<'a, M: PageTableMeta, PTE: GenericPTE, H: PagingHandler>
where
    [(); M::LEVELS]: Sized,
{
    table: &'a mut PageTable<M, PTE>,
    flusher: TlbFlusher<M>,
    _handler: PhantomData<H>,
}
```

- [ ] **Step 2: Add `PageTable::cursor`**

Inside the existing `impl<M, PTE> PageTable<M, PTE>` block, after `base_paddr`, add:

```rust
pub fn cursor<H: PagingHandler>(&mut self) -> PageTableCursor<'_, M, PTE, H> {
    PageTableCursor {
        table: self,
        flusher: TlbFlusher::None,
        _handler: PhantomData,
    }
}
```

- [ ] **Step 3: Add cursor flush methods**

Below the `PageTable` impl block, add this impl:

```rust
impl<M: PageTableMeta, PTE: GenericPTE, H: PagingHandler> PageTableCursor<'_, M, PTE, H>
where
    [(); M::LEVELS]: Sized,
{
    fn push_flush_addr(&mut self, vaddr: M::VirtAddr) {
        match &mut self.flusher {
            TlbFlusher::None => {
                let mut pages = HeaplessVec::new();
                let _ = pages.push(vaddr);
                self.flusher = TlbFlusher::Pages(pages);
            }
            TlbFlusher::Pages(pages) => {
                if pages.push(vaddr).is_err() {
                    self.flusher = TlbFlusher::Full;
                }
            }
            TlbFlusher::Full => {}
        }
    }

    fn push_flush_all(&mut self) {
        self.flusher = TlbFlusher::Full;
    }

    fn record_flush_request(&mut self, request: FlushRequest<M>) {
        match request {
            FlushRequest::None => {}
            FlushRequest::Addr(vaddr) => self.push_flush_addr(vaddr),
            FlushRequest::All => self.push_flush_all(),
        }
    }

    pub fn flush(&mut self) {
        match &self.flusher {
            TlbFlusher::None => {}
            TlbFlusher::Pages(pages) => {
                for vaddr in pages.iter().copied() {
                    M::flush_tlb(Some(vaddr));
                }
            }
            TlbFlusher::Full => M::flush_tlb(None),
        }
        self.flusher = TlbFlusher::None;
    }
}

impl<M: PageTableMeta, PTE: GenericPTE, H: PagingHandler> Drop for PageTableCursor<'_, M, PTE, H>
where
    [(); M::LEVELS]: Sized,
{
    fn drop(&mut self) {
        self.flush();
    }
}
```

- [ ] **Step 4: Move `map` and `unmap` methods to the cursor**

Remove public `PageTable::map` and `PageTable::unmap` from the `PageTable` impl. Add these methods to the `PageTableCursor` impl:

```rust
pub fn map(
    &mut self,
    vaddr: M::VirtAddr,
    paddr: PhysAddr,
    size: usize,
    flags: MappingFlags,
) -> PagingResult {
    let offset = usize::wrapping_sub(vaddr.into(), paddr.into());
    self.iter_pages_in_range(AddrRange::new(vaddr, vaddr + size), |level, _index, page_vaddr, entry| {
        let page_paddr = PhysAddr::from_usize(page_vaddr.wrapping_sub(offset).into());
        *entry = GenericPTE::new_page(page_paddr, flags, level != 0);
        Ok(FlushRequest::Addr(page_vaddr))
    })
}

pub fn unmap(&mut self, vaddr: M::VirtAddr, size: usize) -> PagingResult {
    self.iter_pages_in_range(AddrRange::new(vaddr, vaddr + size), |_level, _index, _page_vaddr, _entry| {
        Ok(FlushRequest::None)
    })
}
```

This step will not compile until the mutating helpers are moved in the next step.

- [ ] **Step 5: Move mutating helpers into the cursor**

Move these helper functions from the `PageTable` impl to the `PageTableCursor` impl and adjust receiver/calls:

```rust
fn get_page_entry_mut(
    &mut self,
    vaddr: M::VirtAddr,
    level: usize,
    create_if_not_exists: bool,
    split_huge_page: bool,
) -> PagingResult<(&mut PTE, usize)>
```

```rust
fn next_table_mut<'a, const LEVEL: usize>(
    &mut self,
    entry: &mut PTE,
    create_if_not_exists: bool,
    split_huge_page: bool,
) -> PagingResult<&'a mut [PTE]>
```

```rust
fn clear_pte(&mut self, entry: &mut PTE, level: usize, vaddr: M::VirtAddr) -> PagingResult
```

```rust
fn iter_pages_in_range<F>(
    &mut self,
    range: AddrRange<M::VirtAddr>,
    mut f: F,
) -> PagingResult
where
    F: FnMut(usize, usize, M::VirtAddr, &mut PTE) -> PagingResult<FlushRequest<M>>,
```

Use these call adjustments in moved helpers:

```rust
PageTable::<M, PTE>::table_of_mut::<LEVEL, H>(paddr)
PageTable::<M, PTE>::table_of_mut_non_const::<H>(paddr, level)
PageTable::<M, PTE>::alloc_table::<LEVEL, H>()
PageTable::<M, PTE>::index::<LEVEL>(vaddr)
self.table.root
self.clear_pte(entry, level, page_vaddr)?
self.next_table_mut::<LEVEL>(entry, create_if_not_exists, split_huge_page)?
```

In `clear_pte`, implement flush recording like this:

```rust
if entry.is_unused() {
    Ok(())
} else if level == 0 || entry.is_huge() {
    if entry.is_huge() {
        self.push_flush_all();
    } else {
        self.push_flush_addr(vaddr);
    }
    entry.clear();
    Ok(())
} else {
    let table = PageTable::<M, PTE>::table_of_mut_non_const::<H>(entry.paddr(), level - 1);
    for (index, child) in table.iter_mut().enumerate() {
        let child_vaddr = vaddr + index * M::LEVEL_PAGE_SIZE[level - 1];
        self.clear_pte(child, level - 1, child_vaddr)?;
    }
    Ok(())
}
```

- [ ] **Step 6: Fix the huge-page split table target while moving helpers**

When moving `next_table_mut`, replace the current split branch with this code. This fixes the table-fill target and adds full-flush tracking.

```rust
} else if entry.is_huge() {
    if split_huge_page {
        let old_paddr = entry.paddr();
        let flags = entry.flags();

        let table_paddr = PageTable::<M, PTE>::alloc_table::<LEVEL, H>()?;
        *entry = GenericPTE::new_table(table_paddr);

        // A huge-page TLB entry may cover any address in the old mapping.
        // Use a conservative full flush when replacing a huge leaf with a table.
        self.push_flush_all();

        let table = PageTable::<M, PTE>::table_of_mut::<LEVEL, H>(table_paddr);
        for (i, entry) in table.iter_mut().enumerate() {
            *entry = PTE::new_page(old_paddr + i * M::LEVEL_PAGE_SIZE[LEVEL], flags, LEVEL != 0);
        }

        Ok(table)
    } else {
        Err(PagingError::MappedToHugePage)
    }
}
```

- [ ] **Step 7: Run cursor tests**

Run:

```bash
cargo test -p expt cursor_drop_flushes_single_page cursor_manual_flush_is_idempotent_with_drop
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add expt/src/lib.rs expt/Cargo.toml Cargo.lock
git commit -m "feat(expt): add page table cursor"
```

If `Cargo.lock` did not change, omit it.

---

### Task 5: Add Functional Mapping Tests

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add single-page and range mapping tests**

Append these tests to the test module:

```rust
#[test]
fn cursor_maps_and_unmaps_single_base_page() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x3000),
                PhysAddr::from_usize(0xA000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
        cursor.flush();
        cursor.unmap(VirtAddr::from_usize(0x3000), TestMeta::PAGE_SIZE).unwrap();
    }

    assert_eq!(lookup(&table, 0x3000), None);
    assert_eq!(flush_log(), std::vec![Some(0x3000), Some(0x3000)]);
    reset_test_state();
}

#[test]
fn cursor_maps_unaligned_range_with_virtual_physical_offset() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x1803),
                PhysAddr::from_usize(0x9803),
                0x2800,
                MappingFlags::READ,
            )
            .unwrap();
    }

    assert_eq!(lookup(&table, 0x1000).unwrap().paddr, 0x9000);
    assert_eq!(lookup(&table, 0x2000).unwrap().paddr, 0xA000);
    assert_eq!(lookup(&table, 0x3000).unwrap().paddr, 0xB000);
    assert_eq!(lookup(&table, 0x4000).unwrap().paddr, 0xC000);
    assert_eq!(lookup(&table, 0x5000), None);
    reset_test_state();
}
```

- [ ] **Step 2: Add largest-page selection and overwrite tests**

Append these tests:

```rust
#[test]
fn cursor_uses_largest_possible_page_levels() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x10000),
                TestMeta::LEVEL_PAGE_SIZE[2],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map(
                VirtAddr::from_usize(0x10000),
                PhysAddr::from_usize(0x20000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    assert_eq!(lookup(&table, 0x0000).unwrap().page_size, TestMeta::LEVEL_PAGE_SIZE[2]);
    assert_eq!(lookup(&table, 0x8000).unwrap().page_size, TestMeta::LEVEL_PAGE_SIZE[2]);
    assert_eq!(lookup(&table, 0x10000).unwrap().page_size, TestMeta::LEVEL_PAGE_SIZE[1]);
    reset_test_state();
}

#[test]
fn cursor_overwrites_existing_mappings() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x10000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x30000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    let mapping = lookup(&table, 0x2000).unwrap();
    assert_eq!(mapping.paddr, 0x32000);
    assert_eq!(mapping.flags, MappingFlags::READ | MappingFlags::WRITE);
    assert_eq!(mapping.page_size, TestMeta::LEVEL_PAGE_SIZE[1]);
    reset_test_state();
}
```

- [ ] **Step 3: Run the functional tests and verify failures**

Run:

```bash
cargo test -p expt cursor_maps_and_unmaps_single_base_page cursor_maps_unaligned_range_with_virtual_physical_offset cursor_uses_largest_possible_page_levels cursor_overwrites_existing_mappings
```

Expected: If Task 4 moved the existing behavior correctly, these may pass immediately. If they fail, the failure should identify an incorrect flush, page-size choice, offset calculation, or clear behavior.

- [ ] **Step 4: Fix implementation only if these tests fail**

If page-size choice or offset fails, inspect `iter_pages_in_range` and `map` in `expt/src/lib.rs`. The mapping loop must keep this logic:

```rust
let mut start_vaddr = range.start.align_down(M::LEVEL_PAGE_SIZE[0]);
let end_vaddr = range.end.align_up(M::LEVEL_PAGE_SIZE[0]);

while start_vaddr < end_vaddr {
    for level in (0..=M::MAX_PAGE_LEVEL).rev() {
        let page_size = M::LEVEL_PAGE_SIZE[level];
        if start_vaddr.is_aligned(page_size) && (start_vaddr + page_size) <= end_vaddr {
            let (entry, index) = self.get_page_entry_mut(start_vaddr, level, true, true)?;
            let flush = f(level, index, start_vaddr, entry)?;
            self.record_flush_request(flush);
            start_vaddr = start_vaddr + page_size;
            break;
        }
    }
}
```

If overwrite fails, make sure `get_page_entry_mut` calls `self.clear_pte(...)` before returning the selected PTE for replacement.

- [ ] **Step 5: Run all `expt` tests**

Run:

```bash
cargo test -p expt
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add expt/src/lib.rs
git commit -m "test(expt): cover cursor mapping behavior"
```

---

### Task 6: Add Huge Page Tests And Conservative Full Flush Behavior

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add huge-page split tests**

Append these tests:

```rust
#[test]
fn splitting_huge_page_preserves_unaffected_subpages_and_full_flushes() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x40000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        clear_flush_log();
        cursor
            .map(
                VirtAddr::from_usize(0x2000),
                PhysAddr::from_usize(0x90000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    assert_eq!(flush_log(), std::vec![None]);
    assert_eq!(lookup(&table, 0x0000).unwrap().paddr, 0x40000);
    assert_eq!(lookup(&table, 0x1000).unwrap().paddr, 0x41000);
    assert_eq!(lookup(&table, 0x2000).unwrap().paddr, 0x90000);
    assert_eq!(lookup(&table, 0x3000).unwrap().paddr, 0x43000);
    reset_test_state();
}

#[test]
fn unmapping_huge_leaf_records_full_flush() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x4000),
                PhysAddr::from_usize(0x50000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        clear_flush_log();
        cursor
            .unmap(VirtAddr::from_usize(0x4000), TestMeta::LEVEL_PAGE_SIZE[1])
            .unwrap();
    }

    assert_eq!(lookup(&table, 0x4000), None);
    assert_eq!(flush_log(), std::vec![None]);
    reset_test_state();
}
```

- [ ] **Step 2: Run the huge-page tests**

Run:

```bash
cargo test -p expt splitting_huge_page_preserves_unaffected_subpages_and_full_flushes unmapping_huge_leaf_records_full_flush
```

Expected: PASS if Task 4 already implemented `push_flush_all()` in split and huge clear. If it fails, continue to Step 3.

- [ ] **Step 3: Fix huge-page clear behavior**

In cursor `clear_pte`, the leaf branch must distinguish normal leaf and huge leaf exactly as follows:

```rust
} else if level == 0 || entry.is_huge() {
    if entry.is_huge() {
        self.push_flush_all();
    } else {
        self.push_flush_addr(vaddr);
    }
    entry.clear();
    Ok(())
}
```

- [ ] **Step 4: Fix huge-page split behavior**

In cursor `next_table_mut`, the split branch must call `self.push_flush_all()` immediately after replacing the huge leaf with a table PTE. The code comment must be present:

```rust
// A huge-page TLB entry may cover any address in the old mapping.
// Use a conservative full flush when replacing a huge leaf with a table.
self.push_flush_all();
```

- [ ] **Step 5: Run all `expt` tests**

Run:

```bash
cargo test -p expt
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add expt/src/lib.rs
git commit -m "test(expt): cover huge page cursor flushing"
```

---

### Task 7: Add Threshold, Partial Failure, And Pressure Tests

**Files:**
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Add threshold fallback test**

Append this test:

```rust
#[test]
fn cursor_flush_threshold_falls_back_to_full_flush() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        for page in 0..33 {
            cursor
                .map(
                    VirtAddr::from_usize(page * TestMeta::PAGE_SIZE),
                    PhysAddr::from_usize(0x100000 + page * TestMeta::PAGE_SIZE),
                    TestMeta::PAGE_SIZE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
    }

    assert_eq!(flush_log(), std::vec![None]);
    reset_test_state();
}
```

- [ ] **Step 2: Add partial failure test**

Append this test. With three-level `TestMeta`, root allocation is allocation 1, then mapping a sparse range can force a later table allocation to fail after an earlier page was installed.

```rust
#[test]
fn partial_success_still_flushes_before_returned_error() {
    reset_test_state();
    let mut table = new_table();
    fail_on_alloc(2);

    let result = {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor.map(
            VirtAddr::from_usize(0x0000),
            PhysAddr::from_usize(0x200000),
            TestMeta::LEVEL_PAGE_SIZE[2] + TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
    };

    assert!(matches!(result, Err(PagingError::AllocationFailed)));
    assert!(lookup(&table, 0x0000).is_some());
    assert_eq!(lookup(&table, TestMeta::LEVEL_PAGE_SIZE[2]), None);
    assert_eq!(flush_log(), std::vec![Some(0x0000)]);
    reset_test_state();
}
```

- [ ] **Step 3: Add deterministic pressure test**

Append this test:

```rust
#[test]
fn deterministic_pressure_matches_shadow_model() {
    reset_test_state();
    let mut table = new_table();
    let mut shadow = [None::<usize>; 64];
    let mut seed = 0x1234_5678usize;

    for step in 0..256 {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        let page = (seed >> 8) % shadow.len();
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        let len_pages = 1 + ((seed >> 16) % 4);
        let max_len = core::cmp::min(len_pages, shadow.len() - page);
        let vaddr = page * TestMeta::PAGE_SIZE;
        let size = max_len * TestMeta::PAGE_SIZE;

        if step % 3 == 0 {
            {
                let mut cursor = table.cursor::<TestPagingHandler>();
                cursor.unmap(VirtAddr::from_usize(vaddr), size).unwrap();
            }
            for item in &mut shadow[page..page + max_len] {
                *item = None;
            }
        } else {
            let paddr = 0x400000 + step * 0x10000;
            {
                let mut cursor = table.cursor::<TestPagingHandler>();
                cursor
                    .map(
                        VirtAddr::from_usize(vaddr),
                        PhysAddr::from_usize(paddr),
                        size,
                        MappingFlags::READ | MappingFlags::WRITE,
                    )
                    .unwrap();
            }
            for (offset, item) in shadow[page..page + max_len].iter_mut().enumerate() {
                *item = Some(paddr + offset * TestMeta::PAGE_SIZE);
            }
        }

        for (page_index, expected) in shadow.iter().enumerate() {
            let vaddr = page_index * TestMeta::PAGE_SIZE;
            let actual = lookup(&table, vaddr).map(|mapping| mapping.paddr);
            assert_eq!(actual, *expected, "step {step}, page {page_index}");
        }
    }

    reset_test_state();
}
```

- [ ] **Step 4: Run these tests and verify failures or pass**

Run:

```bash
cargo test -p expt cursor_flush_threshold_falls_back_to_full_flush partial_success_still_flushes_before_returned_error deterministic_pressure_matches_shadow_model
```

Expected: PASS after cursor tracking and mapping semantics are correct. If threshold fallback fails, continue to Step 5.

- [ ] **Step 5: Fix threshold fallback if needed**

Ensure `push_flush_addr` does not keep the first 32 addresses after overflow. It must switch to `Full` and emit only `M::flush_tlb(None)` during `flush()`:

```rust
TlbFlusher::Pages(pages) => {
    if pages.push(vaddr).is_err() {
        self.flusher = TlbFlusher::Full;
    }
}
```

- [ ] **Step 6: Run all `expt` tests**

Run:

```bash
cargo test -p expt
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add expt/src/lib.rs
git commit -m "test(expt): cover cursor flush edge cases"
```

---

### Task 8: Migrate `ecraos` Call Site And Enforce Cursor-Only Mutation

**Files:**
- Modify: `ecraos/src/mem.rs`
- Modify: `expt/src/lib.rs`

- [ ] **Step 1: Replace direct map calls with one cursor**

In `ecraos/src/mem.rs`, replace lines around the physical-region loop with this shape:

```rust
{
    let mut cursor = early_page_table.cursor::<early::EarlyPagingHandler>();
    for region in pmm::phys_mem_regions() {
        let mapping_flags = region_flags_to_mapping(region.flags);
        if mapping_flags.is_empty() {
            continue;
        }

        let paddr = region.range.start;
        let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
        let vaddr_high = vaddr_low + virt_phys_offset;
        let size = region.range.size();

        cursor.map(vaddr_low, paddr, size, mapping_flags).unwrap();
        cursor.map(vaddr_high, paddr, size, mapping_flags).unwrap();
    }
}
```

The explicit block drops the cursor before `early_page_table.base_paddr()` is used to load `cr3`.

- [ ] **Step 2: Confirm `PageTable::map` and `PageTable::unmap` are gone**

Run:

```bash
rg -n "pub fn map|pub fn unmap|\.map::<|\.unmap::<" expt/src/lib.rs ecraos/src/mem.rs
```

Expected:

```text
expt/src/lib.rs:<line>:    pub fn map(
expt/src/lib.rs:<line>:    pub fn unmap(
```

The remaining `pub fn map` and `pub fn unmap` must be inside the `impl PageTableCursor` block, and there must be no `.map::<...>` or `.unmap::<...>` call in `ecraos/src/mem.rs`.

- [ ] **Step 3: Run checks**

Run:

```bash
cargo test -p expt
cargo check -p ecraos
```

Expected: `cargo test -p expt` PASS. `cargo check -p ecraos` PASS unless the local kernel target configuration blocks checking; if it fails for target reasons, capture the first concrete compiler error in the task result.

- [ ] **Step 4: Commit**

```bash
git add ecraos/src/mem.rs expt/src/lib.rs
git commit -m "refactor(ecraos): use expt page table cursor"
```

---

### Task 9: Final Review And Verification

**Files:**
- Review: `expt/src/lib.rs`
- Review: `expt/src/meta.rs`
- Review: `expt/Cargo.toml`
- Review: `ecraos/src/mem.rs`

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all
```

Expected: exits successfully.

- [ ] **Step 2: Run final tests**

Run:

```bash
cargo test -p expt
```

Expected: PASS.

- [ ] **Step 3: Run downstream check**

Run:

```bash
cargo check -p ecraos
```

Expected: PASS, or a documented target/environment failure unrelated to the cursor API.

- [ ] **Step 4: Inspect cursor safety invariants manually**

Run:

```bash
rg -n "push_flush_all|push_flush_addr|is_huge\(|new_table|clear_pte|iter_pages_in_range" expt/src/lib.rs
```

Expected observations:

- `push_flush_all` is called in the huge-page split path.
- `push_flush_all` is called when clearing a huge leaf.
- `push_flush_addr` is called after installing a new page mapping.
- `push_flush_addr` is called when clearing a non-huge leaf.
- `clear_pte` receives a virtual address parameter.
- public mutation methods live on `PageTableCursor`, not `PageTable`.

- [ ] **Step 5: Check git state**

Run:

```bash
git status --short
```

Expected: no uncommitted changes after all task commits. If `cargo fmt --all` changed files in Step 1, commit those formatting-only changes:

```bash
git add expt/src/lib.rs expt/src/meta.rs ecraos/src/mem.rs expt/Cargo.toml Cargo.lock
git commit -m "style: format expt cursor changes"
```
