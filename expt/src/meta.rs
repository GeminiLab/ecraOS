use core::{fmt::LowerHex, ops::Add};

use memory_addr::MemoryAddr;

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

pub const fn level_end_bit<M: PageTableMeta + ?Sized>(level: usize) -> usize
where
    [(); M::LEVELS]: Sized,
{
    level_start_bit::<M>(level + 1)
}

pub trait PageTableMeta: Send + Sync {
    type VirtAddr: MemoryAddr + Add<usize, Output = Self::VirtAddr> + LowerHex;

    /// Flushes the local TLB.
    ///
    /// `Some(vaddr)` flushes the entry for the mapping containing `vaddr`.
    /// `None` flushes the whole local TLB.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>);

    // Required constants:
    /// The number of levels in the page table.
    const LEVELS: usize;
    /// The number of bits in the page offset, also the log2 of the page size.
    const PAGE_OFFSET_BITS: usize;
    /// The number of bits handled by each level of the page table.
    ///
    /// Note that `LEVEL_BITS[0]` is the number of bits handled by the last
    /// level of the page table, and `LEVEL_BITS[LEVELS - 1]` is the number of
    /// bits handled by the first level of the page table. For exmaple, 32-bit
    /// x86 PAE page tables have `[9, 9, 2]`, and RISC-V Sv39x4 page tables have
    /// `[9, 9, 11]`.
    const LEVEL_BITS: [usize; Self::LEVELS] where [(); Self::LEVELS]: Sized;

    // Required constants with default values:
    /// The maximum level of the page table whose entries can be a page.
    ///
    /// The value must satisfy `0 <= MAX_PAGE_LEVEL < LEVELS`.
    const MAX_PAGE_LEVEL: usize = 0;

    // Derived constants:
    /// The size of non-huge pages.
    const PAGE_SIZE: usize = 1 << Self::PAGE_OFFSET_BITS;
    /// The ranges of bits `(start, end)` handled by each level of the page
    /// table. `start` is inclusive and `end` is exclusive.
    ///
    /// See [`Self::LEVEL_BITS`] for more details.
    const LEVEL_BIT_RANGES: [(usize, usize); Self::LEVELS] = {
        let mut ranges = [(0, 0); Self::LEVELS];
        let mut index = 0;
        while index < Self::LEVELS {
            ranges[index] = (level_start_bit::<Self>(index), level_end_bit::<Self>(index));
            index += 1;
        }
        ranges
    } where [(); Self::LEVELS]: Sized;
    /// The number of entries in each level of the page table.
    ///
    /// See [`Self::LEVEL_BITS`] for more details.
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
    const VA_BITS: usize = {
        let mut sum = Self::PAGE_OFFSET_BITS;
        let mut index = 0;
        while index < Self::LEVELS {
            sum += Self::LEVEL_BITS[index];
            index += 1;
        }
        sum
    } where [(); Self::LEVELS]: Sized;
    /// The size of pages pointed by each level of the page table.
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
