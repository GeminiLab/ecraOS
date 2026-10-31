# `exslab`

`exslab` is a `no_std` slab allocator for objects whose size and alignment are at most 2048 bytes.
It allocates no memory of its own. Page allocation, virtual mappings, locking, and per-CPU
allocator selection belong to the caller.

## Concepts

- A **slot** is the region reserved for one object. It may be larger than the requested size.
- A **size class** is one of nine power-of-two slot sizes from 8 to 2048 bytes.
  [`SizeClass::from_layout`] selects the smallest class that satisfies both the size and the
  alignment of an allocation layout ([`core::alloc::Layout`]).
- A **slab** is a memory region aligned to its own size. It holds a
  [`SlabPageHeader`](slab_page::SlabPageHeader), allocation bitmaps, and a fixed number of slots
  of one size class. **Slab memory** is the complete region, supplied by the caller.
- A **cache** is a list of slabs of one size class.
- The **owner** of a slab is the allocator or standalone cache that registered it. Each slab also
  records a logical CPU identifier, `owner_cpu`, which the caller uses to route frees.

## Usage

This host example obtains slab memory from the standard allocator. A kernel instead supplies
physical pages and passes their mapped virtual address, with the same complete-span alignment.

```rust
use std::{alloc::{alloc_zeroed, dealloc, Layout}, ptr::NonNull};

use exslab::{SlabAllocResult, SlabAllocator, SlabDeallocResult};
use memory_addr::VirtAddr;

let mut allocator = SlabAllocator::new(4096).unwrap();
let layout = Layout::from_size_align(64, 8).unwrap();

// The first allocation reports that this size class needs slab memory.
let SlabAllocResult::NeedsSlab { size_class, pages } = allocator.alloc(layout).unwrap() else {
    unreachable!();
};
let span = pages * 4096;
let slab_memory_layout = Layout::from_size_align(span, span).unwrap();
let slab_memory = NonNull::new(unsafe { alloc_zeroed(slab_memory_layout) }).unwrap();
let base = VirtAddr::from_mut_ptr_of(slab_memory.as_ptr());

// SAFETY: The allocation supplies exclusive writable slab memory of the requested span, aligned
// to that span.
unsafe { allocator.add_slab(size_class, base, 0) }.unwrap();

let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() else {
    unreachable!();
};
// SAFETY: The object is live and retains its allocation layout and hardware page size.
assert_eq!(unsafe { SlabAllocator::owner_cpu(object, layout, 4096) }, Ok(0));
// SAFETY: The object is a live allocation of this allocator, released once with its layout.
assert!(matches!(
    unsafe { allocator.dealloc(object, layout) },
    SlabDeallocResult::Done
));

// One empty slab stays cached, so drop the allocator before releasing the example's slab memory.
drop(allocator);
// SAFETY: All slab users have finished and the allocation layout is unchanged.
unsafe { dealloc(slab_memory.as_ptr(), slab_memory_layout) };
```

## Allocating and freeing

[`SlabAllocator`] keeps one cache per size class. [`SlabAllocator::alloc`] selects the class from
the allocation layout and takes a slot from that cache. When the cache cannot supply a slot, the
call returns [`SlabAllocResult::NeedsSlab`] with the class and exact page count. The caller then
allocates that many contiguous mapped writable pages aligned to the complete span, registers them
with [`SlabAllocator::add_slab`], and retries. Registration takes no size argument because the
class determines the slab size. A size or alignment above 2048 bytes returns
[`AllocError::InvalidParam`].

Each cache keeps a list of partial slabs, a list of full slabs, and at most one retained empty
slab. Partial slabs have locally available slots and full slabs have none. A full slab may still
hold slots freed remotely and not yet drained, so allocation checks full slabs for pending remote
frees before reporting that it needs a slab.

[`SlabAllocator::dealloc`] returns [`SlabDeallocResult::Done`] while the slab stays cached. When a
slab becomes empty while another empty slab is already retained, the cache removes the newly
emptied slab and returns its base address and exact page count through
[`SlabDeallocResult::FreeSlab`]. The caller then releases that memory.

[`SlabCache`](cache::SlabCache) provides the same operations for one caller-selected slot and slab
size. [`SlabAllocator`] is the usual entry point.

## Local and remote frees

Local operations register slab memory, allocate slots, and free slots through the owner. They take
`&mut` access to prevent concurrent operations. However, Rust's mutable reference does not prevent
concurrent operations from the interrupt handler and other tasks on the same CPU. It's the caller's
responsibility to serialize them.

A remote free does not borrow the owner. [`SlabAllocator::remote_free_object`] pushes the released
slot onto an atomic list in its slab header, using the slot's first machine word as the link. The
owner detaches the whole list when a later allocation or deallocation uses that slab, and restores
the slots to the local bitmap. Slots pushed after a detachment stay pending for a later drain.
[`SlabPageHeader::remote_free`](slab_page::SlabPageHeader::remote_free) documents the atomic
protocol and memory orderings.

To choose a path, the caller reads the slab's owner with [`SlabAllocator::owner_cpu`]. It needs
only the object address, the original allocation layout, and the hardware page size. It locates
the header by masking the address and borrows no allocator state, so any CPU can call it. A kernel
compares the result with the current CPU identifier and calls either its locked local path or
[`SlabAllocator::remote_free_object`].

Both local and remote frees require a live allocation, freed exactly once with its original
allocation layout. A remote free also requires the hardware page size used by the owner. After a
remote free publishes the slot, the owner may reuse the slot or reclaim the slab memory at any
time. A remote free never returns slab memory itself.

Neither an allocator nor a cache releases slab memory when dropped. The caller keeps slab memory
valid until it is returned, or until every cache operation, live slot, and remote free has
finished. The `# Safety` section of each operation states its exact requirements.

## Slab memory

For slot size `C` and hardware page size `P`, a slab occupies `max(min(64 KiB, C * 512), P)` bytes
and must be aligned to that size. [`SlabAllocator::new`] accepts 4, 16, or 64 KiB hardware pages.

| Slot size | Pages with 4 KiB pages | Pages with 16 KiB pages | Pages with 64 KiB pages |
| --- | ---: | ---: | ---: |
| 8 B | 1 | 1 | 1 |
| 16 B | 2 | 1 | 1 |
| 32 B | 4 | 1 | 1 |
| 64 B | 8 | 2 | 1 |
| 128 B through 2048 B | 16 | 4 | 1 |

The header and bitmaps share the slab with the slots, so `C * 512` does not guarantee 512 slots.
With 4 KiB pages, an 8-byte slab holds 496 slots and a 2048-byte slab holds 31. The last slot ends
exactly at the end of the slab.

```text
slab base, aligned to the slab size
      |
      v
+--------------------------------------+
| SlabPageHeader, 32 bytes             |
+--------------------------------------+
| Summary bitmap, reserved, 32 bytes   |
+--------------------------------------+
| First-level bitmap, one bit per slot |
+--------------------------------------+
| Padding up to the slot alignment     |
+--------------------------------------+
| N slots of the class size            |
+--------------------------------------+  <-- slab end, no trailing padding
```

The first-level bitmap records which slots are locally available. A slab with more than 512 slots
also uses the summary bitmap, with one bit per group of 512 slots, so a search scans at most eight
first-level words. Large hardware pages therefore are not limited to 512 slots. For example, an
8-byte slab in a 64 KiB page holds 8058 slots. The [`slab_page`] module documents exact offsets
and bit meanings.

## Precomputed layouts

A [`SlabLayout`] record describes the slot offset, slot count, and bitmap sizes for one slot size
and slab size. Records for all 63 combinations of the nine slot sizes and power-of-two slab sizes
from 4 to 256 KiB are computed during compilation. [`cache::SlabCache::new`] selects a record by
the base-2 logarithms of the two sizes, and [`SlabAllocator`] selects records by its page-size
policy. Each cache binds one record for its entire lifetime. One shared implementation reads the
record at runtime, instead of generic instantiations or function pointers for each layout.

## Validation and performance

Host tests cover all 63 slab layouts, all nine classes with 4, 16, and 64 KiB pages, writes to
every byte of allocated slots, exhaustion and bitmap restoration, exact reclamation, and concurrent
remote publication. QEMU runs exercise the kernel integration separately.

