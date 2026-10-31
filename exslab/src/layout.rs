//! Slab layout records.
//!
//! A layout record describes the offset of size of the slot region in the slab. It's determined by
//! the combination of slot size and slab size. Every supported combination is computed at compile
//! time into [`LAYOUTS`]. A cache resolves one record when it is constructed and keeps using it.
//!
//! It's so designed that allocation code could be shared and only reads the record at runtime.
//! Benchmarks show that this design is faster than a design that computes the record at runtime,
//! and also faster than a design that uses the layout as a const generic parameter.

use crate::size_class::SizeClass;

/// Number of bytes in one kibibyte.
const BYTES_PER_KIB: usize = 1024;
/// Size of the slab header region.
pub(crate) const SLAB_HEADER_SIZE: usize = 32;
/// Starting byte offset of the reserved summary bitmap.
pub(crate) const SUMMARY_BITMAP_OFFSET: usize = SLAB_HEADER_SIZE;
/// Size in bytes of the reserved summary bitmap.
pub(crate) const SUMMARY_BITMAP_SIZE: usize = 32;
/// Starting byte offset of the first-level allocation bitmap.
pub(crate) const FIRST_LEVEL_BITMAP_OFFSET: usize = SUMMARY_BITMAP_OFFSET + SUMMARY_BITMAP_SIZE;
/// Number of bits in one byte.
const BITS_PER_BYTE: usize = u8::BITS as usize;
/// Number of bits in each bitmap word.
pub(crate) const BITS_PER_BITMAP_WORD: usize = u64::BITS as usize;
/// Number of object slots described by one summary bitmap bit.
pub(crate) const SUMMARY_GROUP_SLOT_COUNT: usize = 512;
/// Number of first-level bitmap words described by one summary bitmap bit.
pub(crate) const BITMAP_WORDS_PER_SUMMARY_GROUP: usize =
    SUMMARY_GROUP_SLOT_COUNT / BITS_PER_BITMAP_WORD;
/// Number of allocation-state bits required for each object slot.
const ALLOCATION_BITS_PER_SLOT: usize = 1;

/// Base-2 logarithm of the smallest supported slot size in bytes.
const MIN_SLOT_LOG2: usize = SizeClass::Bytes8.size().trailing_zeros() as usize;
/// Base-2 logarithm of the largest supported slot size in bytes.
const MAX_SLOT_LOG2: usize = SizeClass::Bytes2048.size().trailing_zeros() as usize;
/// Smallest supported slab size, 4 KiB.
const MIN_SLAB_SIZE: usize = 4 * BYTES_PER_KIB;
/// Largest supported slab size, 256 KiB.
const MAX_SLAB_SIZE: usize = 256 * BYTES_PER_KIB;
/// Base-2 logarithm of the smallest supported slab size in bytes.
const MIN_SLAB_LOG2: usize = MIN_SLAB_SIZE.trailing_zeros() as usize;
/// Base-2 logarithm of the largest supported slab size in bytes.
const MAX_SLAB_LOG2: usize = MAX_SLAB_SIZE.trailing_zeros() as usize;
/// Number of supported slab sizes.
const SLAB_SIZE_COUNT: usize = MAX_SLAB_LOG2 - MIN_SLAB_LOG2 + 1;
/// Number of slots targeted by the allocator's slab-size policy.
const TARGET_SLOT_COUNT: usize = 512;
/// Slab-size cap applied before enforcing the hardware page-size minimum.
const MAX_POLICY_SLAB_SIZE: usize = 64 * BYTES_PER_KIB;

/// A slab layout.
///
/// The record is computed at compile time for one slot size and slab size, and a cache binds one
/// record for its entire lifetime. Slabs store no pointer to their record. The cache supplies the
/// record, and masking a live slot address with it locates the slab base.
#[derive(Clone, Copy, Debug)]
pub struct SlabLayout {
    /// Complete slab size and required alignment in bytes.
    pub slab_size: usize,
    /// Byte offset from the slab base to the first object slot.
    pub slot_offset: usize,
    /// Number of complete object slots ending at the slab boundary.
    pub slot_count: usize,
    /// Address mask that clears the low bits of a slot address, leaving the slab base.
    pub(crate) slab_mask: usize,
    /// Base-2 logarithm of the slot size in bytes, used to index slots by shifting.
    pub(crate) object_log2: usize,
    /// Number of active first-level bitmap words.
    pub(crate) bitmap_words: usize,
    /// Number of active summary words, or zero when a single bitmap level suffices.
    pub(crate) summary_words: usize,
    /// Size class written into each initialized slab header.
    pub(crate) size_class: SizeClass,
}

/// Precomputed slab layout records for supported object and slab sizes.
///
/// The outer index is the slab-size exponent minus 12, and the inner index is the size-class index.
static LAYOUTS: [[SlabLayout; SizeClass::SIZE_CLASS_COUNT]; SLAB_SIZE_COUNT] = build_layouts();

/// Construction and selection of supported slab layouts.
impl SlabLayout {
    /// Computes the maximal aligned slot region for a supported pair of exponents.
    ///
    /// The exponents are the base-2 logarithms of the slot and slab sizes in bytes. The result
    /// keeps one allocation bit per slot inside the slab, rounds the slot region up to the slot
    /// alignment, and ends the last slot exactly at the slab boundary. This function runs
    /// during compilation.
    const fn compute(object_log2: usize, slab_log2: usize) -> Self {
        let object_size = 1usize << object_log2;
        let slab_size = 1usize << slab_log2;
        // Each slot needs its object storage plus one allocation bit.
        let mut slot_count = (slab_size - FIRST_LEVEL_BITMAP_OFFSET) * BITS_PER_BYTE
            / (object_size * BITS_PER_BYTE + ALLOCATION_BITS_PER_SLOT);
        while slot_count > 0 {
            let slot_offset = (FIRST_LEVEL_BITMAP_OFFSET + slot_count.div_ceil(BITS_PER_BYTE))
                .next_multiple_of(object_size);
            if slot_offset + slot_count * object_size <= slab_size {
                assert!(slot_count <= u16::MAX as usize);
                assert!(slot_offset + slot_count * object_size == slab_size);
                return Self {
                    slab_size,
                    slot_offset,
                    slot_count,
                    slab_mask: !(slab_size - 1),
                    object_log2,
                    bitmap_words: slot_count.div_ceil(BITS_PER_BITMAP_WORD),
                    summary_words: if slot_count > SUMMARY_GROUP_SLOT_COUNT {
                        slot_count
                            .div_ceil(SUMMARY_GROUP_SLOT_COUNT)
                            .div_ceil(BITS_PER_BITMAP_WORD)
                    } else {
                        0
                    },
                    size_class: SizeClass::ALL[object_log2 - MIN_SLOT_LOG2],
                };
            }
            slot_count -= 1;
        }
        panic!("a supported slab layout must fit at least one slot")
    }

    /// Selects a slab layout by slot and slab size exponents.
    ///
    /// The exponents are the base-2 logarithms of the slot and slab sizes in bytes. Returns `None`
    /// unless the slot size is a supported power of two from 8 to 2048 bytes and the slab size is
    /// a supported power of two from 4 to 256 KiB.
    pub(crate) const fn select(object_log2: usize, slab_log2: usize) -> Option<&'static Self> {
        if object_log2 < MIN_SLOT_LOG2
            || object_log2 > MAX_SLOT_LOG2
            || slab_log2 < MIN_SLAB_LOG2
            || slab_log2 > MAX_SLAB_LOG2
        {
            return None;
        }
        Some(&LAYOUTS[slab_log2 - MIN_SLAB_LOG2][object_log2 - MIN_SLOT_LOG2])
    }

    /// Selects the allocator's slab layout for one size class.
    ///
    /// The layout targets 512 slots, capped at a 64 KiB slab, but never uses a slab smaller than
    /// one hardware page. `page_log2` is the base-2 logarithm of that page size in bytes, and
    /// the caller must validate that it is 4, 16, or 64 KiB before calling.
    pub(crate) const fn for_class(class: SizeClass, page_log2: usize) -> &'static Self {
        let object_log2 = class.index() + MIN_SLOT_LOG2;
        let candidate = object_log2 + TARGET_SLOT_COUNT.ilog2() as usize;
        let max_policy_slab_log2 = MAX_POLICY_SLAB_SIZE.ilog2() as usize;
        let capped = if candidate < max_policy_slab_log2 {
            candidate
        } else {
            max_policy_slab_log2
        };
        let slab_log2 = if capped > page_log2 {
            capped
        } else {
            page_log2
        };
        &LAYOUTS[slab_log2 - MIN_SLAB_LOG2][class.index()]
    }
}

/// Builds all supported slab layout records.
///
/// The table covers every supported slot and slab size and is evaluated during compilation, so no
/// runtime allocation or initialization is needed.
const fn build_layouts() -> [[SlabLayout; SizeClass::SIZE_CLASS_COUNT]; SLAB_SIZE_COUNT] {
    let mut table = [[SlabLayout::compute(MIN_SLOT_LOG2, MIN_SLAB_LOG2);
        SizeClass::SIZE_CLASS_COUNT]; SLAB_SIZE_COUNT];
    let mut slab = 0;
    while slab < table.len() {
        let mut class = 0;
        while class < SizeClass::SIZE_CLASS_COUNT {
            table[slab][class] = SlabLayout::compute(class + MIN_SLOT_LOG2, slab + MIN_SLAB_LOG2);
            class += 1;
        }
        slab += 1;
    }
    table
}
