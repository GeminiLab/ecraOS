# exbuddy

exbuddy provides a no_std physical page allocator based on the buddy system. It manages multiple
disjoint physical memory sections, stores its metadata inside each section, and uses a fixed virtual
to physical address offset to access that metadata.

## Model

Each managed section contains a header, an array of one metadata record per heap page, and the
allocatable heap:

```text
+------------------------------+
| BuddySection                 |
+------------------------------+
| PageMeta for each heap page  |
+------------------------------+
| Allocatable heap             |
+------------------------------+
```

The allocator is no_std. Its section metadata is initialized in place, so adding a section is an
unsafe operation that requires a valid, writable fixed-offset mapping for the complete physical
region.

The allocator keeps free blocks in intrusive lists indexed by buddy order. A block's buddy is
calculated from its absolute physical frame number, so blocks remain naturally aligned even when a
section starts at a nonzero physical frame number. The maximum supported buddy order is
[`MAX_ORDER`]. Exact frame allocations can span several buddy blocks within one section.

## Usage

The allocator needs a page size shift and a fixed mapping offset. The caller owns the memory used
for section storage and must keep it valid for as long as the allocator can access the section:

```rust,ignore
use exbuddy::BuddyAllocator;

let mut allocator = BuddyAllocator::new();
unsafe { allocator.init(12, virtual_physical_offset)? };
unsafe { allocator.add_section(physical_memory_range)? };

let frame = allocator.alloc_frame()?;
allocator.dealloc_frame(frame)?;
```

[`BuddyAllocator::alloc_block`] allocates one power-of-two buddy block.
[`BuddyAllocator::alloc_frames`] allocates an exact number of contiguous frames and can fall back to
adjacent free blocks. The `*_at` methods operate on a caller-selected page-aligned range and require
the range to remain within one managed section.
