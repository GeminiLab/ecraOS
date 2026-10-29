# `exslab`

`exslab` provides a `no_std` slab allocator for objects whose size and alignment are at most
2048 bytes. It uses caller-supplied backing and performs no heap allocation of its own. Page
allocation, virtual mappings, locking, and per-CPU allocator selection belong to the caller.

## Size classes and backing

[`SizeClass::from_layout`] selects the smallest power-of-two class satisfying both the requested
size and alignment. Nine classes range from 8 to 2048 bytes. [`SlabAllocator::new`] accepts 4, 16,
or 64 KiB hardware pages and rejects other sizes before accepting backing.

For class size `C` and hardware page size `P`, the complete slab size is
`max(min(64 KiB, C * 512), P)`. Each slab must be aligned to its complete size.

| Object size | Pages with 4 KiB pages | Pages with 16 KiB pages | Pages with 64 KiB pages |
| --- | ---: | ---: | ---: |
| 8 B | 1 | 1 | 1 |
| 16 B | 2 | 1 | 1 |
| 32 B | 4 | 1 | 1 |
| 64 B | 8 | 2 | 1 |
| 128 B through 2048 B | 16 | 4 | 1 |

[`SlabAllocResult::NeedsSlab`] reports the exact page count and class. The caller allocates that
many contiguous mapped writable pages, aligned to `pages * P`, then registers them with
[`SlabAllocator::add_slab`] and retries. Registration takes no independent size argument. Its
unsafe contract transfers exclusive use of the complete backing span to the allocator.

## Precomputed layouts

All 63 combinations of object exponents 3 through 11 and slab exponents 12 through 18 are
computed during compilation. [`cache::SlabCache::new`] can select any of these standalone
geometries. The allocator's page-size policy selects the combinations it needs from this table.

Each cache binds an immutable [`SlabLayout`] record for its entire lifetime. The record contains
the slab size and address mask, object exponent, slot offset and capacity, and bitmap lengths.
Allocation and deallocation use shared cache and page code, with runtime data selecting the
geometry. There are no generic page instantiations or geometry-specific function pointers.

| Byte range | Contents |
| --- | --- |
| `0..32` | Common header, including owner, links, counts, and atomic remote head |
| `32..64` | Reserved second-level bitmap |
| Starting at `64` | First-level bitmap, one bit per object slot |
| Next object-size boundary through slab end | Naturally aligned object slots |

Capacity is the largest positive slot count whose bitmap and aligned object region fit. The
object region ends exactly at the slab boundary, with no trailing padding. Metadata and alignment
consume part of the backing, so the `C * 512` policy does not promise 512 usable slots.

Slabs with more than 512 slots also use one summary bit per group of 512 slots. Selecting a
group limits the subsequent search to at most eight first-level `u64` words. This permits small
objects to use large hardware pages without imposing the old 512-slot capacity limit. For example,
8-byte slots in a 64 KiB slab provide 8058 slots. No per-slab layout pointer or magic marker is used.

## Usage

This host example supplies backing through the standard allocator. A kernel supplies physical
pages and passes their mapped virtual address with the same complete-span alignment.

```rust
use core::{alloc::Layout, ptr::NonNull};
use std::alloc::{alloc_zeroed, dealloc};

use exslab::{SlabAllocResult, SlabAllocator, SlabDeallocResult};
use memory_addr::VirtAddr;

let mut allocator = SlabAllocator::new(4096).unwrap();
let layout = Layout::from_size_align(64, 8).unwrap();
let SlabAllocResult::NeedsSlab { size_class, pages } = allocator.alloc(layout).unwrap() else {
    unreachable!();
};
let span = pages * 4096;
let backing_layout = Layout::from_size_align(span, span).unwrap();
let backing = NonNull::new(unsafe { alloc_zeroed(backing_layout) }).unwrap();
let base = VirtAddr::from_mut_ptr_of(backing.as_ptr());

// SAFETY: The allocation supplies exclusive writable backing with the requested alignment.
unsafe { allocator.add_slab(size_class, base, 0) }.unwrap();
let SlabAllocResult::Allocated(object) = allocator.alloc(layout).unwrap() else {
    unreachable!();
};
// SAFETY: This live object retains its original layout and hardware page size.
assert_eq!(unsafe { SlabAllocator::owner_cpu(object, layout, 4096) }, Ok(0));
// SAFETY: This is a live allocation from this allocator, released once with its original layout.
assert!(matches!(
    unsafe { allocator.dealloc(object, layout) },
    SlabDeallocResult::Done
));

// One empty slab is cached. Discard the allocator before releasing the example's backing.
drop(allocator);
// SAFETY: All slab users have finished and the allocation layout is unchanged.
unsafe { dealloc(backing.as_ptr(), backing_layout) };
```

## Local and remote frees

The caller serializes all local allocation, registration, and deallocation for an allocator. Each
class cache has partial and full lists plus one retained empty slab. List membership follows the
local bitmap, so a full slab may still contain pending remote frees. Allocation drains published
slots before using them. Local deallocation also drains remote frees before deciding whether a
slab is empty. A second empty slab is removed and returned through
[`SlabDeallocResult::FreeSlab`] with the exact backing page count.

[`SlabAllocator::owner_cpu`] locates the immutable owner using the original allocation layout and
hardware page size. Complete-span alignment permits direct address masking. It neither scans
candidate headers nor borrows the owning allocator. The kernel uses this result to choose its
locked local path or [`SlabAllocator::remote_free_object`].

A remote producer uses the released slot's first machine word as a link and publishes it through
the header's atomic head. The owner detaches a batch with an acquire swap and restores its slots
to the local bitmap. Publication after detachment remains visible in the head for a later drain.
Remote operations never access the owner's cache state. All concurrent header access projects
individual fields through raw pointers, avoiding references that cover owner-managed fields.

Every release requires a live allocation freed exactly once with its original layout and hardware
page size. Backing must remain live until successful remote publication. The owner may then reuse
the slot or reclaim its backing immediately. A remote release does not itself return backing.

Neither an allocator nor a standalone cache releases backing on `Drop`. The caller retains backing
until returned, or until all cache operations, live slots, and remote producers have finished.

## Validation and performance evidence

Host tests cover all 63 standalone geometries, all nine classes with 4/16/64 KiB pages, complete
payload writes, exhaustion and bitmap restoration, exact reclamation, and concurrent remote
publication. The kernel integration is exercised separately through QEMU.

The sibling `exslab_comparison` branch retains five experimental implementations, the benchmark,
repeat scripts, and full comparison reports. Its report records 15 sweeps and 16,200 samples.
Those measurements describe the frozen experimental executable, rather than this production
build. The layout-data variant showed strong mixed-request results on the measured Ryzen 5 5500,
but was not fastest in every case. Compiler output, code placement, request distribution, CPU
features, and performance across architectures still require careful testing.
