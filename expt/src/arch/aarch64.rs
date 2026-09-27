//! AArch64 page-table metadata and TLB operations.
//!
//! The module describes the standard 4 KiB granule with four translation levels
//! and performs inner-shareable stage-one TLB invalidation.

use aarch64_cpu::asm::barrier::{ISH, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::{PageTableCoverage, PageTableMeta};

/// Metadata for an AArch64 page table.
///
/// `BITS` selects the virtual-address width and `COVERAGE` selects the lower,
/// upper, or symmetric portion represented by the table.
pub struct AArch64PageTableMeta<
    const BITS: usize,
    const PAGE_OFFSET_BITS: usize,
    const COVERAGE: u8,
>;

/// The minimal possible `T<n>SZ` value we support.
///
/// When `FEAT_LPA2` is supported, the minimal `T<n>SZ` value is 12, giving a
/// 52-bit virtual address space.
pub const TNSZ_MIN: usize = 12;
/// The maximal possible `T<n>SZ` value we support.
///
/// When `FEAT_TTST` is supported, the maximal `T<n>SZ` value could actually
/// be higher (48 for 4 KiB and 16 KiB granules and 47 for 64 KiB granules). But
/// we limit it to 39 for practical purposes.
pub const TNSZ_MAX: usize = 39;
/// The size of AArch64 virtual addresses in bits.
pub const ADDR_BITS: usize = 64;

/// The base-2 logarithm of the size of a 4 KiB page (12).
pub const PAGE_OFFSET_BITS_4KIB: usize = 12;
/// The base-2 logarithm of the size of a 16 KiB page (14).
pub const PAGE_OFFSET_BITS_16KIB: usize = 14;
/// The base-2 logarithm of the size of a 64 KiB page (16).
pub const PAGE_OFFSET_BITS_64KIB: usize = 16;
/// The base-2 logarithm of the size of a page-table entry (3 for 8-byte entries).
pub const PAGE_TABLE_ENTRY_SIZE_BITS: usize = 3;

impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, const COVERAGE: u8>
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, COVERAGE>
{
    /// The number of virtual-address bits consumed by each level in the page
    /// table. This equals to the base-2 logarithm of the number of entries per
    /// page table level.
    const EACH_LEVEL_BITS: usize = {
        match PAGE_OFFSET_BITS {
            PAGE_OFFSET_BITS_4KIB | PAGE_OFFSET_BITS_16KIB | PAGE_OFFSET_BITS_64KIB => {
                PAGE_OFFSET_BITS - PAGE_TABLE_ENTRY_SIZE_BITS
            }
            _ => panic!("Unsupported page size"),
        }
    };

    /// The number of virtual-address bits used for the page index.
    const PAGE_INDEX_BITS: usize = BITS.strict_sub(PAGE_OFFSET_BITS);
}

impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, const COVERAGE: u8> PageTableMeta
    for AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, COVERAGE>
{
    type VirtAddr = VirtAddr;

    const LEVELS: usize = {
        if BITS > (ADDR_BITS - TNSZ_MIN) || BITS < (ADDR_BITS - TNSZ_MAX) {
            panic!("Unsupported number of bits for AArch64 4 KiB page table");
        }

        Self::PAGE_INDEX_BITS.div_ceil(Self::EACH_LEVEL_BITS)
    };

    const PAGE_OFFSET_BITS: usize = PAGE_OFFSET_BITS;

    const LEVEL_BITS: [usize; Self::LEVELS] = {
        let mut result = [Self::EACH_LEVEL_BITS; Self::LEVELS];
        let remainder =  Self::PAGE_INDEX_BITS.rem_euclid(Self::EACH_LEVEL_BITS);

        if remainder != 0 {
            *result.last_mut().unwrap() = remainder;
        }

        // Verify that the sum of the level bits and the page offset bits equals
        // the total number of bits.
        let mut sum = PAGE_OFFSET_BITS;
        let mut index = 0;
        while index < Self::LEVELS {
            sum += result[index];
            index += 1;
        }

        if sum != BITS {
            panic!("Level bits do not sum up to the total number of bits");
        }

        result
    } where [(); Self::LEVELS]: Sized;

    const COVERAGE: PageTableCoverage = {
        match PageTableCoverage::from_u8(COVERAGE) {
            PageTableCoverage::Symmetric => {
                panic!("Symmetric coverage is not supported for AArch64 4 KiB page tables")
            }
            c => c,
        }
    };

    const MAX_PAGE_LEVEL: usize = {
        if Self::LEVELS < 3 {
            Self::LEVELS - 1
        } else {
            2
        }
    };

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        unsafe {
            match vaddr {
                Some(vaddr) => core::arch::asm!(
                    "tlbi vaae1is, {address}",
                    address = in(reg) vaddr.as_usize() >> 12,
                    options(nostack)
                ),
                None => core::arch::asm!("tlbi vmalle1is", options(nostack)),
            }

            dsb(ISH);
            isb(SY);
        }
    }
}

pub type AArch64PageTableMeta4KiB<const BITS: usize, const COVERAGE: u8> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_4KIB, COVERAGE>;
pub type AArch64PageTableMeta16KiB<const BITS: usize, const COVERAGE: u8> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_16KIB, COVERAGE>;
pub type AArch64PageTableMeta64KiB<const BITS: usize, const COVERAGE: u8> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_64KIB, COVERAGE>;

pub type AArch64PageTableMeta4KiBLower<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, { PageTableCoverage::Lower as u8 }>;
pub type AArch64PageTableMeta4KiBUpper<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, { PageTableCoverage::Upper as u8 }>;
pub type AArch64PageTableMeta16KiBLower<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, { PageTableCoverage::Lower as u8 }>;
pub type AArch64PageTableMeta16KiBUpper<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, { PageTableCoverage::Upper as u8 }>;
pub type AArch64PageTableMeta64KiBLower<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, { PageTableCoverage::Lower as u8 }>;
pub type AArch64PageTableMeta64KiBUpper<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, { PageTableCoverage::Upper as u8 }>;

/// Metadata for AArch64 4 KiB, four-level, 48-bit translation tables.
///
/// Levels zero through two may contain leaf mappings, providing 4 KiB, 2 MiB,
/// and 1 GiB mapping sizes.
pub struct Aarch64PageTableMeta;

impl PageTableMeta for Aarch64PageTableMeta {
    /// The standard virtual-address type used by AArch64 page tables.
    type VirtAddr = VirtAddr;

    /// The four translation levels in the 48-bit format.
    const LEVELS: usize = 4;
    /// The twelve offset bits in a 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9; Self::LEVELS];
    /// AArch64 four-level paging covers only the lower portion of the virtual address space.
    ///
    /// This is not correct and only a placeholder for now.
    const COVERAGE: PageTableCoverage = PageTableCoverage::Lower;

    /// The highest level that supports a block mapping.
    ///
    /// Level two maps 1 GiB blocks, while level three remains table-only.
    const MAX_PAGE_LEVEL: usize = 2;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// The operation uses inner-shareable stage-one invalidation followed by the
    /// required data and instruction synchronization barriers.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        unsafe {
            match vaddr {
                Some(vaddr) => core::arch::asm!(
                    "tlbi vaae1is, {address}",
                    address = in(reg) vaddr.as_usize() >> 12,
                    options(nostack)
                ),
                None => core::arch::asm!("tlbi vmalle1is", options(nostack)),
            }

            dsb(ISH);
            isb(SY);
        }
    }
}
