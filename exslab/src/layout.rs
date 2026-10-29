//! Compile-time geometry records used by a single runtime page implementation.

use crate::size_class::SizeClass;

/// Immutable geometry for a naturally aligned object region and its bitmaps.
///
/// All records are computed at compile time. A cache binds one record for its entire lifetime.
#[derive(Clone, Copy, Debug)]
pub struct SlabLayout {
    /// Complete backing size and alignment in bytes.
    pub slab_size: usize,
    /// Byte offset from the slab base to the first object slot.
    pub slot_offset: usize,
    /// Number of complete object slots ending at the slab boundary.
    pub slot_count: usize,
    /// Address mask locating the slab base from a live object.
    pub(crate) slab_mask: usize,
    /// Object-size exponent used for runtime slot-address shifts.
    pub(crate) object_log2: usize,
    /// Number of active first-level bitmap words.
    pub(crate) bitmap_words: usize,
    /// Number of active summary words, or zero for a single-level bitmap.
    pub(crate) summary_words: usize,
    /// Size class written into each initialized common header.
    pub(crate) size_class: SizeClass,
}

/// Precomputed records for nine object sizes and seven backing sizes from 4 to 256 KiB.
static LAYOUTS: [[SlabLayout; SizeClass::SIZE_CLASS_COUNT]; 7] = build_layouts();

impl SlabLayout {
    /// Computes the maximal aligned slot region for a supported pair of exponents.
    const fn compute(object_log2: usize, slab_log2: usize) -> Self {
        let object_size = 1usize << object_log2;
        let slab_size = 1usize << slab_log2;
        let mut slot_count = (slab_size - 64) * 8 / (object_size * 8 + 1);
        while slot_count > 0 {
            let slot_offset = (64 + slot_count.div_ceil(8)).next_multiple_of(object_size);
            if slot_offset + slot_count * object_size <= slab_size {
                assert!(slot_count <= u16::MAX as usize);
                assert!(slot_offset + slot_count * object_size == slab_size);
                return Self {
                    slab_size,
                    slot_offset,
                    slot_count,
                    slab_mask: !(slab_size - 1),
                    object_log2,
                    bitmap_words: slot_count.div_ceil(64),
                    summary_words: if slot_count > 512 {
                        slot_count.div_ceil(512).div_ceil(64)
                    } else {
                        0
                    },
                    size_class: SizeClass::ALL[object_log2 - 3],
                };
            }
            slot_count -= 1;
        }
        panic!("a supported geometry must fit at least one slot")
    }

    /// Selects a precomputed record after validating both exponents.
    pub(crate) const fn select(object_log2: usize, slab_log2: usize) -> Option<&'static Self> {
        if object_log2 < 3 || object_log2 > 11 || slab_log2 < 12 || slab_log2 > 18 {
            return None;
        }
        Some(&LAYOUTS[slab_log2 - 12][object_log2 - 3])
    }

    /// Selects the policy geometry for one class and a validated hardware page exponent.
    pub(crate) const fn for_class(class: SizeClass, page_log2: usize) -> &'static Self {
        let object_log2 = class.index() + 3;
        let candidate = object_log2 + 9;
        let capped = if candidate < 16 { candidate } else { 16 };
        let slab_log2 = if capped > page_log2 {
            capped
        } else {
            page_log2
        };
        &LAYOUTS[slab_log2 - 12][class.index()]
    }
}

/// Builds every supported record during compilation without allocating at runtime.
const fn build_layouts() -> [[SlabLayout; SizeClass::SIZE_CLASS_COUNT]; 7] {
    let mut table = [[SlabLayout::compute(3, 12); SizeClass::SIZE_CLASS_COUNT]; 7];
    let mut slab = 0;
    while slab < table.len() {
        let mut class = 0;
        while class < SizeClass::SIZE_CLASS_COUNT {
            table[slab][class] = SlabLayout::compute(class + 3, slab + 12);
            class += 1;
        }
        slab += 1;
    }
    table
}
