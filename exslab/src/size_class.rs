//! Slot size classes for the slab allocator.

use core::alloc::Layout;

/// The slot sizes supported by the slab allocator.
///
/// Classes double from 8 to 2048 bytes. Allocation layouts are rounded up to the smallest size
/// class that can satisfy the layout's size and alignment requirements.
///
///
/// We keep discriminants as compact cache indices because allocation and deallocation use `index()`
/// on their hot paths. A size-valued representation makes `size()` cheaper but requires a bit scan
/// for `index()`, which slowed the measured x86_64 release slab allocation and deallocation paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SizeClass {
    /// 8-byte object slots.
    Bytes8 = 0,
    /// 16-byte object slots.
    Bytes16 = 1,
    /// 32-byte object slots.
    Bytes32 = 2,
    /// 64-byte object slots.
    Bytes64 = 3,
    /// 128-byte object slots.
    Bytes128 = 4,
    /// 256-byte object slots.
    Bytes256 = 5,
    /// 512-byte object slots.
    Bytes512 = 6,
    /// 1024-byte object slots.
    Bytes1024 = 7,
    /// 2048-byte object slots.
    Bytes2048 = 8,
}

/// The largest object size and alignment that the slab allocator accepts.
pub const SLAB_MAX_SIZE: usize = 2048;

/// Selection and properties of supported object size classes.
impl SizeClass {
    /// All size classes in ascending order.
    pub const ALL: [SizeClass; Self::SIZE_CLASS_COUNT] = [
        SizeClass::Bytes8,
        SizeClass::Bytes16,
        SizeClass::Bytes32,
        SizeClass::Bytes64,
        SizeClass::Bytes128,
        SizeClass::Bytes256,
        SizeClass::Bytes512,
        SizeClass::Bytes1024,
        SizeClass::Bytes2048,
    ];

    /// Number of distinct size classes.
    pub const SIZE_CLASS_COUNT: usize = 9;

    /// Slot sizes in ascending size-class order.
    const CLASS_SIZES: [usize; Self::SIZE_CLASS_COUNT] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048];

    /// Selects the smallest size class that can satisfy an allocation layout.
    ///
    /// Rounds up the larger of the requested size and alignment up to the smallest size class,
    /// because a slot must satisfy both. Returns `None` if that value exceeds [`SLAB_MAX_SIZE`].
    pub fn from_layout(layout: Layout) -> Option<SizeClass> {
        let size = layout.size().max(layout.align());
        if size > SLAB_MAX_SIZE {
            return None;
        }
        let index = size.max(8).next_power_of_two().trailing_zeros() as usize - 3;
        Some(Self::ALL[index])
    }

    // Performance experiments on 2026-10-28 compared the loop and this bit-based expression in
    // standalone classification and local allocation/deallocation, with varied input distributions
    // and default, forced, or disabled inlining. Faster classification did not consistently mean
    // faster allocation, and disabling inlining did not eliminate the reversal.
    //
    // On a Ryzen 5 5500, nightly-2026-06-01 release builds for generic x86-64 emitted BSR + XOR
    // rather than LZCNT. Later runs pinned the CPU, warmed the allocator, counted only the hot
    // region, and interleaved variants. For uniform inputs, median cycles per allocation/free pair
    // were 96.4 for the loop and 103.5 for this expression, despite fewer retired instructions
    // (327.6 versus 297.4). Keeping one object live per slab preserved the slowdown, so repeatedly
    // moving empty slabs between cache lists was not its sole cause.
    //
    // Replacing only BSR + XOR with equivalent, equally sized LZCNT + NOP sequences in the same
    // binary preserved instruction counts, addresses, and register allocation, while reducing the
    // bit-based path to about 95.4 cycles per pair. Dependent instruction microbenchmarks measured
    // about 5 versus 1 cycles and 7 versus 2 retired macro-ops per sequence on this CPU. The scan
    // result feeds cache-address calculation. Replacing the allocation and free sites separately
    // saved about 4 cycles each for uniform inputs. Lower IPC also reflected fewer instructions
    // doing the same work, so IPC alone did not establish a slowdown. Fixed-size requests still
    // favored the loop after replacement, and enabling the LZCNT target feature changed codegen.
    //
    // Prefer this expression for clarity, not as a proven performance optimum. Finding the fastest
    // implementation still requires careful testing of realistic workloads, allocator state,
    // compiler output, and CPU features, especially across x86-64, AArch64, and RISC-V.

    /// Returns the slot size in bytes for this size class.
    pub const fn size(self) -> usize {
        Self::CLASS_SIZES[self as usize]
    }

    /// Returns the array index (0-based) for this size class.
    pub const fn index(self) -> usize {
        self as usize
    }
}
