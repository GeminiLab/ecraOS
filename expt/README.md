# `expt`

Multi-level page table structures and operations for ecraOS.

This crate provides:

- [`PageTableMeta`](meta::PageTableMeta) and [`GenericPTE`](pte::GenericPTE) for describing page
  table formats in a static and type-safe manner.
- [`PageTable`](pt::PageTable) and [`DualPageTable`](pt::DualPageTable) for representing page table
  hierarchies.
- [`PendingCleanup`](pt::cleanup::PendingCleanup) and [`TlbFlusher`](pt::cleanup::TlbFlusher) for
  tracking and processing TLB invalidations.
- predefined metadata and TLB flushing strategies for [supported architectures](arch).

This crate is `no_std` and uses a caller-supplied [`PageAllocator`](expalloc_trait::PageAllocator)
for table frame allocation and deallocation. It requires nightly Rust and is developed with the
toolchain pinned in the repository's `rust-toolchain.toml`.

## Defining Page Table Formats

The format of a page table hierarchy is described by [`PageTableMeta`](meta::PageTableMeta),
including the base page size, number of levels, bits per level, huge-page support, and
virtual-address coverage. The format of a page table entry is described by [`GenericPTE`](pte::GenericPTE).

For example, the following code defines the RISC-V 64 Sv48 page table format:

```rust
use expt::meta::{PageTableMeta, SymmetricCoverage};
use memory_addr::VirtAddr;

struct Sv48PageTableMeta;

impl PageTableMeta for Sv48PageTableMeta {
    type VirtAddr = VirtAddr;
    const LEVELS: usize = 4;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];
    type Coverage = SymmetricCoverage<Self::VirtAddr>;
    const MAX_PAGE_LEVEL: usize = 3;
}
```

The [`arch`] module supplies metadata and local TLB flushing strategies for the current target:

| Architecture | Metadata | Base page sizes | Virtual-address coverage |
| --- | --- | --- | --- |
| x86-64 | `X86Level4PageTableMeta`, `X86Level5PageTableMeta` | 4 KiB | Symmetric |
| RISC-V 64 | `Sv39PageTableMeta`, `Sv48PageTableMeta`, `Sv57PageTableMeta` | 4 KiB | Symmetric |
| AArch64 | `AArch64PageTableMeta` and granule aliases | 4, 16, or 64 KiB | Lower or upper |

```rust
use expt::{arch::x86_64::X86Level4PageTableMeta, meta::PageTableMeta};

type Meta = X86Level4PageTableMeta;

assert_eq!(<Meta as PageTableMeta>::LEVELS, 4);
assert_eq!(<Meta as PageTableMeta>::PAGE_OFFSET_BITS, 12);
assert_eq!(<Meta as PageTableMeta>::LEVEL_BITS, [9, 9, 9, 9]);
assert_eq!(<Meta as PageTableMeta>::MAX_PAGE_LEVEL, 2);
assert_eq!(<Meta as PageTableMeta>::PAGE_SIZE, 1 << 12);
assert_eq!(<Meta as PageTableMeta>::VA_BITS, 48);
assert_eq!(<Meta as PageTableMeta>::LEVEL_BIT_RANGES, [(12, 21), (21, 30), (30, 39), (39, 48)]);
assert_eq!(<Meta as PageTableMeta>::LEVEL_TABLE_SIZE, [1 << 9; 4]);
assert_eq!(
    <Meta as PageTableMeta>::LEVEL_PAGE_SIZE,
    [1 << 12, 1 << 21, 1 << 30, 1 << 39]
);
```

## Constructing Page Table Types

The [`PageTable`](pt::PageTable) struct is the primary type for representing a page table hierarchy.
It is parameterized by:

- The metadata type, which describes the page table format.
- The page table entry type, which describes the page table entries.
- The allocator type, which is used to allocate and deallocate the page table frames.
- The TLB flusher type, which is used to flush the TLB.

For example, the following code defines the x86-64 level 4 and level 5 page table types.

```rust,no_run
use expt::{
    arch::x86_64::{
        X86Level4PageTableMeta,
        X86Level5PageTableMeta,
        X86LocalTlbFlusher,
    },
    pt::PageTable,
    pte::x86_64::X64PTE,
};

type X86Level4PageTable<AL> =
    PageTable<X86Level4PageTableMeta, X64PTE, AL, X86LocalTlbFlusher>;
type X86Level5PageTable<AL> =
    PageTable<X86Level5PageTableMeta, X64PTE, AL, X86LocalTlbFlusher>;
```

For AArch64 where two hierarchies are used for lower and upper address spaces respectively, this
crate provides [`DualPageTable`](pt::DualPageTable) for representing two independent hierarchies.
It's basically a wrapper around two [`PageTable`](pt::PageTable)s, one for the lower address space
and one for the upper address space, and provides a unified interface for both hierarchies.

```rust,no_run
use expt::pt::DualPageTable;
use memory_addr::VirtAddr;

type AArch64PageTables<ML, PTEL, MU, PTEU, AL, F> = DualPageTable<
    VirtAddr,
    ML,
    PTEL,
    MU,
    PTEU,
    AL,
    F,
>;
```

For scenarios where the page table format should be selected at runtime, this crate provides
[`OpaquePageTableType`](opaque::OpaquePageTableType) for capturing and storing concrete page table
formats in a single type and [`OpaquePageTable`](opaque::OpaquePageTable) for creating a page table
hierarchy from a type captured by [`OpaquePageTableType`](opaque::OpaquePageTableType).

```rust,no_run
use expalloc_trait::PageAllocator;
use expt::{
    arch::x86_64::{
        X86Level4PageTableMeta,
        X86Level5PageTableMeta,
        X86LocalTlbFlusher,
    },
    opaque::OpaquePageTableType,
    pte::x86_64::X64PTE,
};
use memory_addr::VirtAddr;

fn select_x86_page_table_type<AL>(la57: bool) -> OpaquePageTableType<VirtAddr>
where
    AL: PageAllocator,
{
    if la57 {
        OpaquePageTableType::<VirtAddr>::new::<
            X86Level5PageTableMeta,
            X64PTE,
            AL,
            X86LocalTlbFlusher,
        >()
    } else {
        OpaquePageTableType::<VirtAddr>::new::<
            X86Level4PageTableMeta,
            X64PTE,
            AL,
            X86LocalTlbFlusher,
        >()
    }
}
```

## Mutation and cleanup

Both [`PageTable`](pt::PageTable) and [`DualPageTable`](pt::DualPageTable) use a cursor-based
interface for mutation. [`PageTableCursorLike`](pt::PageTableCursorLike) defines the common
interface for cursors. A cursor batches mappings and unmappings, accumulating TLB invalidations
until `clean_up` processes them or `take_cleanup` transfers them to the caller. Dropping a cursor
processes any remaining cleanup.

Ranges expand outward to base-page boundaries. Mapping replaces existing mappings and chooses the
largest supported leaves allowed by the range and physical-address alignment. Unmapping can split
huge leaves and may allocate intermediate tables even along previously absent paths. Errors retain
earlier mutations. Operations are not transactional.

Opaque `map`, `unmap`, and `apply_actions` process pending cleanup before returning, including
after an action error.
[`OpaquePageTable::apply_actions_with_cleanup`](opaque::OpaquePageTable::apply_actions_with_cleanup)
transfers pending cleanup to the caller only on success. An action error or iterator panic causes
the temporary cursor to process pending cleanup before returning the error or unwinding. The
calling context must therefore permit automatic cleanup on these paths.

## Table-frame lifetime

Multi-level page table structure instances deallocate all table frames in their hierarchies when
dropped. Replacing a child-table entry deallocates its subtree's table frames during the mutation,
without waiting for pending TLB invalidations. Mapped memory ranges remain caller-managed.

Copying a root address does not extend the table frames' lifetime. `leak` consumes a value while
retaining its frames, allowing another compatible value to adopt the hierarchy with `new_at`. Its
allocator must be able to access and deallocate every retained table frame. Callers must quiesce
hardware users before dropping a multi-level page table. Destruction does not invalidate TLBs, and
a mutable Rust borrow does not exclude hardware page walks.

## Safely Updating Page Tables

Page table updates such as `map` and `unmap` do not synchronize with software accesses or
hardware page walks. The caller must enforce the following requirements across all processors
that may use the affected mappings or page tables:

1. Before starting an update, prevent new accesses to the affected virtual-address ranges and
   any page table memory that may be modified or deallocated. Wait for existing accesses to
   complete.
2. During the update, allow accesses to those ranges and page table memory only as needed to
   perform the update or process pending cleanup.
3. Before allowing other accesses again, finish the update and process all pending cleanup,
   including the required TLB invalidations and synchronization. This requirement also applies
   when an update fails after making partial changes.

A mutable Rust borrow prevents conflicting Rust accesses through the borrowed value, but it
does not stop hardware page walks or accesses on other processors. The selected
[`TlbFlusher`](pt::cleanup::TlbFlusher) must provide the invalidation scope and completion
guarantees required by the hierarchy's users.

The mutation methods are not marked `unsafe`. These requirements must instead be upheld by the
system design where the hierarchy is installed for hardware use and subsequent accesses are
controlled.

## Terminology

The terms **multi-level page table** and **multi-level page table structure** refer to the Rust
abstraction representing translation across all levels, such as [`PageTable`](pt::PageTable) or
[`DualPageTable`](pt::DualPageTable), and to values of those types. A **page table hierarchy** is
the corresponding in-memory data structure traversed by hardware. A **page table** is one table at a
single level within a hierarchy. **Table frames**, also called **page table frames**, are the
physical frames storing those tables. One page table may occupy one or more table frames.

The convention above does not affect the meaning of the architecture-specific terms, e.g. Sv39 page
table, x86-64 5-level page table, etc.

## Known issues

RISC-V mapping flags are not validated. Leaf permissions must include `READ` or `EXECUTE`, and
`WRITE` requires `READ`. With debug assertions enabled, constructing a leaf without `READ` or
`EXECUTE` panics. With debug assertions disabled, such an entry above level zero can be mistaken
for a child-table descriptor. Traversal can then access mapped data as a page table and deallocate
data frames as table frames. Callers must supply valid RISC-V leaf permissions until validation
is implemented.
