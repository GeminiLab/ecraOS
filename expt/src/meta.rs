//! Page-table geometry and address-space metadata.
//!
//! This module defines the metadata trait and derives per-level geometry from
//! architecture-provided constants.

use core::{fmt::LowerHex, marker::PhantomData, ops::Add};

use memory_addr::MemoryAddr;

/// Returns the first virtual-address bit handled by a page-table level.
///
/// Level numbering starts at zero for the lowest page-table level. The returned
/// index includes the page-offset bits and the widths of all lower levels.
pub const fn level_start_bit<M: PageTableMeta + ?Sized>(level: usize) -> usize
where
    [(); M::LEVELS]: Sized,
{
    let mut sum = M::PAGE_OFFSET_BITS;
    let mut index = 0;
    while index < level {
        sum += M::LEVEL_BITS[index];
        index += 1;
    }
    sum
}

/// Returns the exclusive end bit handled by a page-table level.
///
/// The result is the start bit of the next level and therefore defines a
/// half-open bit range together with [`level_start_bit`].
pub const fn level_end_bit<M: PageTableMeta + ?Sized>(level: usize) -> usize
where
    [(); M::LEVELS]: Sized,
{
    level_start_bit::<M>(level + 1)
}

/// The meta-information and architecture operations for a specific page-table
/// format.
///
/// Implementations provide the virtual-address type, level widths, supported
/// leaf levels, and local TLB invalidation primitive. The derived constants use
/// those values to describe table sizes and address-bit ranges.
pub trait PageTableMeta: Send + Sync {
    /// The semantic virtual-address type used by this page-table format.
    ///
    /// It must support address arithmetic and hexadecimal formatting in addition
    /// to the common [`MemoryAddr`] operations.
    type VirtAddr: MemoryAddr + Add<usize, Output = Self::VirtAddr> + LowerHex;

    /// Flushes entries from the local translation lookaside buffer.
    ///
    /// `Some(vaddr)` flushes the entry for the mapping containing `vaddr`.
    /// `None` flushes the whole local TLB.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>);

    // Required constants:
    /// The number of levels in the page table.
    ///
    /// Levels are numbered from zero at the lowest table. Implementations must
    /// provide a level between one and five, inclusive.
    const LEVELS: usize;
    /// The number of bits in an in-page offset of the smallest page.
    ///
    /// This value is also the base-2 logarithm of [`Self::PAGE_SIZE`].
    const PAGE_OFFSET_BITS: usize;
    /// The number of bits handled by each level of the page table.
    ///
    /// Note that `LEVEL_BITS[0]` is the number of bits handled by the last
    /// level of the page table, and `LEVEL_BITS[LEVELS - 1]` is the number of
    /// bits handled by the first level of the page table. For example, 32-bit
    /// x86 PAE page tables have `[9, 9, 2]`, and RISC-V Sv39x4 page tables have
    /// `[9, 9, 11]`.
    const LEVEL_BITS: [usize; Self::LEVELS] where [(); Self::LEVELS]: Sized;

    // Required constants with default values:
    /// The maximum level of the page table whose entries can be a page.
    ///
    /// The value must be less than [`Self::LEVELS`]. Level zero page table
    /// entries are always page entries.
    const MAX_PAGE_LEVEL: usize = 0;

    // Derived constants:
    /// The size of the smallest page in bytes.
    ///
    /// The value is derived as `1 << PAGE_OFFSET_BITS`.
    const PAGE_SIZE: usize = 1 << Self::PAGE_OFFSET_BITS;
    /// The virtual-address bit range handled by each page-table level.
    ///
    /// Each `(start, end)` pair is half-open: `start` is inclusive and `end` is
    /// exclusive. See [`Self::LEVEL_BITS`] for the level ordering.
    const LEVEL_BIT_RANGES: [(usize, usize); Self::LEVELS] = {
        let mut ranges = [(0, 0); Self::LEVELS];
        let mut index = 0;
        while index < Self::LEVELS {
            ranges[index] = (level_start_bit::<Self>(index), level_end_bit::<Self>(index));
            index += 1;
        }
        ranges
    } where [(); Self::LEVELS]: Sized;
    /// The number of entries in a table at each level.
    ///
    /// Each value is `1 << LEVEL_BITS[level]`. See [`Self::LEVEL_BITS`] for the
    /// level ordering.
    const LEVEL_TABLE_SIZE: [usize; Self::LEVELS] = {
        let mut sizes = [0; Self::LEVELS];
        let mut index = 0;
        while index < Self::LEVELS {
            sizes[index] = 1 << Self::LEVEL_BITS[index];
            index += 1;
        }
        sizes
    } where [(); Self::LEVELS]: Sized;
    /// The total number of bits in the virtual address space.
    ///
    /// The value includes the page offset and the bit widths of every page-table
    /// level.
    const VA_BITS: usize = {
        let mut sum = Self::PAGE_OFFSET_BITS;
        let mut index = 0;
        while index < Self::LEVELS {
            sum += Self::LEVEL_BITS[index];
            index += 1;
        }
        sum
    } where [(); Self::LEVELS]: Sized;
    /// The byte size of a leaf mapping at each page-table level.
    ///
    /// Level zero maps [`Self::PAGE_SIZE`] bytes. Each higher level multiplies
    /// the previous size by its number of child entries.
    const LEVEL_PAGE_SIZE: [usize; Self::LEVELS] = {
        let mut sizes = [0; Self::LEVELS];
        sizes[0] = 1 << Self::PAGE_OFFSET_BITS;
        let mut index = 1;
        while index < Self::LEVELS {
            sizes[index] = sizes[index - 1] << Self::LEVEL_BITS[index];
            index += 1;
        }
        sizes
    } where [(); Self::LEVELS]: Sized;
}

/// Compile-time validity checks for page-table metadata.
///
/// Referencing [`Self::ASSERTIONS`] verifies the supported virtual-address
/// width, level count, and maximum leaf level for `M`.
#[doc(hidden)]
pub(crate) struct PageTableMetaAssertions<M: PageTableMeta> {
    /// The metadata type checked by the associated assertions.
    ///
    /// The marker carries `M` without storing a runtime value.
    _phantom: PhantomData<M>,
}

impl<M: PageTableMeta> PageTableMetaAssertions<M> {
    /// The assertion that the virtual-address width fits in 64 bits.
    ///
    /// Evaluation fails at compile time when [`PageTableMeta::VA_BITS`] exceeds
    /// the address representation supported by this crate.
    const VA_BITS_ASSERTIONS: () = assert!(
        M::VA_BITS <= 64,
        "Virtual address space must be no more than 64 bits"
    ) where [(); M::LEVELS]: Sized;

    /// The assertion that the page-table level count is supported.
    ///
    /// Traversal currently handles between one and five levels, inclusive.
    const LEVELS_ASSERTIONS: () = assert!(
        M::LEVELS <= 5 && M::LEVELS > 0,
        "Page table level must be no more than 5 and greater than 0"
    );

    /// The assertion that the maximum leaf level exists.
    ///
    /// Since levels are zero-indexed, the value must be strictly less than the
    /// total number of levels.
    const MAX_PAGE_LEVEL_ASSERTIONS: () = assert!(
        M::MAX_PAGE_LEVEL < M::LEVELS,
        "`M::MAX_PAGE_LEVEL` must be greater or equal than 0 and less than `M::LEVELS`"
    ) where [(); M::LEVELS]: Sized;

    /// All compile-time assertions required for a metadata implementation.
    ///
    /// Referencing this constant forces evaluation of each individual assertion.
    #[doc(hidden)]
    #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
    pub const ASSERTIONS: () = {
        let _ = Self::VA_BITS_ASSERTIONS;
        let _ = Self::LEVELS_ASSERTIONS;
        let _ = Self::MAX_PAGE_LEVEL_ASSERTIONS;
    } where [(); M::LEVELS]: Sized;
}
