//! AArch64 page-table metadata and TLB operations.
//!
//! The module describes AArch64 translation-table geometries for the supported
//! granules and performs inner-shareable stage-one TLB invalidation.

use core::marker::PhantomData;

use aarch64_cpu::asm::barrier::{ISH, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::meta::{LowerCoverage, PageTableCoverage, PageTableMeta, UpperCoverage};

/// A coverage policy supported by an AArch64 translation-table root.
///
/// Individual AArch64 roots represent either the lower TTBR0 range or the
/// upper TTBR1 range. A symmetric root is therefore not a valid AArch64 root.
pub trait AArch64Coverage: PageTableCoverage<VirtAddr = VirtAddr> + Send + Sync {}

impl AArch64Coverage for LowerCoverage<VirtAddr> {}
impl AArch64Coverage for UpperCoverage<VirtAddr> {}

/// Metadata for an AArch64 page table.
///
/// `BITS` selects the virtual-address width and `C` selects the type-level
/// coverage policy represented by the table.
pub struct AArch64PageTableMeta<
    const BITS: usize,
    const PAGE_OFFSET_BITS: usize,
    C: AArch64Coverage,
>(PhantomData<C>);

/// The minimal possible `T<n>SZ` value we support.
///
/// When `FEAT_LPA2` is supported, the minimal `T<n>SZ` value is 12, giving a
/// 52-bit virtual address space.
pub const TNSZ_MIN: usize = 12;
/// The maximal possible `T<n>SZ` value we support.
///
/// When `FEAT_TTST` is supported, the maximal `T<n>SZ` value could actually
/// be higher (48 for 4 KiB and 16 KiB granules and 47 for 64 KiB granules). But
/// we limit it to 39 for practical purposes, giving a 25-bit virtual address
/// space.
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

impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, C>
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, C>
where
    C: AArch64Coverage,
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

impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, C> PageTableMeta
    for AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, C>
where
    C: AArch64Coverage + const PageTableCoverage<VirtAddr = VirtAddr>,
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

    /// The type-level coverage policy for this translation-table root.
    type Coverage = C;

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

/// AArch64 page-table metadata with a 4 KiB granule.
pub type AArch64PageTableMeta4KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_4KIB, C>;
/// AArch64 page-table metadata with a 16 KiB granule.
pub type AArch64PageTableMeta16KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_16KIB, C>;
/// AArch64 page-table metadata with a 64 KiB granule.
pub type AArch64PageTableMeta64KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_64KIB, C>;

/// AArch64 4 KiB lower-half page-table metadata.
pub type AArch64PageTableMeta4KiBLower<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 4 KiB upper-half page-table metadata.
pub type AArch64PageTableMeta4KiBUpper<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, UpperCoverage<VirtAddr>>;
/// AArch64 16 KiB lower-half page-table metadata.
pub type AArch64PageTableMeta16KiBLower<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 16 KiB upper-half page-table metadata.
pub type AArch64PageTableMeta16KiBUpper<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, UpperCoverage<VirtAddr>>;
/// AArch64 64 KiB lower-half page-table metadata.
pub type AArch64PageTableMeta64KiBLower<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 64 KiB upper-half page-table metadata.
pub type AArch64PageTableMeta64KiBUpper<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, UpperCoverage<VirtAddr>>;

/// Metadata for AArch64 4 KiB, four-level, 48-bit translation tables.
///
/// Levels zero through two may contain leaf mappings, providing 4 KiB, 2 MiB,
/// and 1 GiB mapping sizes.
pub type Aarch64PageTableMeta = AArch64PageTableMeta4KiBLower<48>;
