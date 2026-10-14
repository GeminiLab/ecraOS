//! AArch64 page table hierarchy metadata and TLB operations.
//!
//! The module describes AArch64 page table hierarchy geometries for the supported
//! granules and performs PE-local stage-one TLB invalidation.

use core::marker::PhantomData;

use aarch64_cpu::asm::barrier::{ISH, ISHST, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::meta::{LowerCoverage, PageTableCoverage, PageTableMeta, UpperCoverage};

/// A coverage policy supported by an AArch64 root page table.
///
/// Individual AArch64 roots represent either the lower TTBR0 range or the
/// upper TTBR1 range. A symmetric root is therefore not a valid AArch64 root.
pub trait AArch64Coverage: PageTableCoverage<VirtAddr = VirtAddr> + Send + Sync {}

impl AArch64Coverage for LowerCoverage<VirtAddr> {}
impl AArch64Coverage for UpperCoverage<VirtAddr> {}

/// Metadata for an AArch64 page table hierarchy.
///
/// `BITS` selects the virtual-address width and `C` selects the type-level
/// coverage policy represented by the page table hierarchy.
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
///
/// The semantic virtual-address representation contains 64 bits.
pub const ADDR_BITS: usize = 64;

/// The base-2 logarithm of the size of a 4 KiB page.
///
/// This granule uses twelve offset bits below the page table indices.
pub const PAGE_OFFSET_BITS_4KIB: usize = 12;
/// The base-2 logarithm of the size of a 16 KiB page.
///
/// This granule uses fourteen offset bits below the page table indices.
pub const PAGE_OFFSET_BITS_16KIB: usize = 14;
/// The base-2 logarithm of the size of a 64 KiB page.
///
/// This granule uses sixteen offset bits below the page table indices.
pub const PAGE_OFFSET_BITS_64KIB: usize = 16;
/// The base-2 logarithm of the size of a page table entry (3 for 8-byte entries).
///
/// Each page table entry occupies eight bytes for every supported granule.
pub const PAGE_TABLE_ENTRY_SIZE_BITS: usize = 3;

impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, C>
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, C>
where
    C: AArch64Coverage,
{
    /// The index width of a full page table at each hierarchy level.
    ///
    /// This is the base-2 logarithm of the number of entries per page table.
    /// The root page table may use fewer bits when the address width requires it.
    const EACH_LEVEL_BITS: usize = {
        match PAGE_OFFSET_BITS {
            PAGE_OFFSET_BITS_4KIB | PAGE_OFFSET_BITS_16KIB | PAGE_OFFSET_BITS_64KIB => {
                PAGE_OFFSET_BITS - PAGE_TABLE_ENTRY_SIZE_BITS
            }
            _ => panic!("Unsupported page size"),
        }
    };

    /// The number of virtual-address bits used for the page index.
    ///
    /// This subtracts the page offset from the full hierarchy's address width.
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

    /// The type-level coverage policy for this page table hierarchy.
    ///
    /// `C` selects the lower or upper canonical address range.
    type Coverage = C;

    const MAX_PAGE_LEVEL: usize = {
        if Self::LEVELS < 3 {
            Self::LEVELS - 1
        } else {
            2
        }
    };

    /// Invalidates translations on the issuing PE only.
    ///
    /// Common acknowledged shootdown owns remote completion independently of the granule.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_local_tlb(vaddr);
    }
}

/// Invalidates one address or all stage-one translations on the current PE.
///
/// DSB orders prior PTE stores before TLBI and completes invalidation before subsequent access.
/// VAAE1 selects the VA's TTBR regime with VA bits 55:12 for every supported runtime granule.
pub fn flush_local_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        dsb(ISHST);
        match vaddr {
            Some(vaddr) => core::arch::asm!(
                "tlbi vaae1, {address}",
                address = in(reg) (vaddr.as_usize() >> 12) & ((1usize << 44) - 1),
                options(nostack)
            ),
            None => core::arch::asm!("tlbi vmalle1", options(nostack)),
        }
        dsb(ISH);
        isb(SY);
    }
}

/// AArch64 page table hierarchy metadata with a 4 KiB granule.
///
/// `BITS` and `C` retain the selected address width and coverage policy.
pub type AArch64PageTableMeta4KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_4KIB, C>;
/// AArch64 page table hierarchy metadata with a 16 KiB granule.
///
/// `BITS` and `C` retain the selected address width and coverage policy.
pub type AArch64PageTableMeta16KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_16KIB, C>;
/// AArch64 page table hierarchy metadata with a 64 KiB granule.
///
/// `BITS` and `C` retain the selected address width and coverage policy.
pub type AArch64PageTableMeta64KiB<const BITS: usize, C> =
    AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS_64KIB, C>;

/// AArch64 4 KiB lower-half page table hierarchy metadata.
///
/// The lower coverage policy selects the TTBR0 address range.
pub type AArch64PageTableMeta4KiBLower<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 4 KiB upper-half page table hierarchy metadata.
///
/// The upper coverage policy selects the TTBR1 address range.
pub type AArch64PageTableMeta4KiBUpper<const BITS: usize> =
    AArch64PageTableMeta4KiB<BITS, UpperCoverage<VirtAddr>>;
/// AArch64 16 KiB lower-half page table hierarchy metadata.
///
/// The lower coverage policy selects the TTBR0 address range.
pub type AArch64PageTableMeta16KiBLower<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 16 KiB upper-half page table hierarchy metadata.
///
/// The upper coverage policy selects the TTBR1 address range.
pub type AArch64PageTableMeta16KiBUpper<const BITS: usize> =
    AArch64PageTableMeta16KiB<BITS, UpperCoverage<VirtAddr>>;
/// AArch64 64 KiB lower-half page table hierarchy metadata.
///
/// The lower coverage policy selects the TTBR0 address range.
pub type AArch64PageTableMeta64KiBLower<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, LowerCoverage<VirtAddr>>;
/// AArch64 64 KiB upper-half page table hierarchy metadata.
///
/// The upper coverage policy selects the TTBR1 address range.
pub type AArch64PageTableMeta64KiBUpper<const BITS: usize> =
    AArch64PageTableMeta64KiB<BITS, UpperCoverage<VirtAddr>>;

#[cfg(test)]
mod tests {
    use super::AArch64PageTableMeta64KiBLower;
    use crate::meta::PageTableMeta;

    /// Checks block sizes for a non-uniform 64 KiB page table hierarchy.
    ///
    /// A partially used root page table must not alter lower-level mapping sizes.
    #[test]
    fn computes_64k_level_page_sizes() {
        type Meta = AArch64PageTableMeta64KiBLower<48>;

        assert_eq!(
            <Meta as PageTableMeta>::LEVEL_PAGE_SIZE,
            [1 << 16, 1 << 29, 1 << 42]
        );
    }
}

/// Metadata for AArch64 4 KiB, four-level, 48-bit page table hierarchies.
///
/// Levels zero through two may contain leaf mappings, providing 4 KiB, 2 MiB,
/// and 1 GiB mapping sizes.
pub type Aarch64PageTableMeta = AArch64PageTableMeta4KiBLower<48>;
