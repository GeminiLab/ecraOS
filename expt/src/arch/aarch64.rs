//! AArch64 page table hierarchy metadata and TLB operations.
//!
//! The module describes AArch64 page table hierarchy geometries for the supported
//! granules and performs PE-local stage-one TLB invalidation.

use core::marker::PhantomData;

use aarch64_cpu::asm::barrier::{ISH, ISHST, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::{
    meta::{LowerCoverage, PageTableCoverage, PageTableMeta, UpperCoverage},
    pt::cleanup::TlbFlusher,
};

/// A coverage policy supported by an AArch64 root page table.
///
/// Individual AArch64 roots represent either the lower TTBR0 range or the
/// upper TTBR1 range. A symmetric root is therefore not a valid AArch64 root.
pub trait AArch64Coverage: PageTableCoverage<VirtAddr = VirtAddr> + Send + Sync {}

/// Admits the lower-address policy used by TTBR0.
impl AArch64Coverage for LowerCoverage<VirtAddr> {}
/// Admits the upper-address policy used by TTBR1.
impl AArch64Coverage for UpperCoverage<VirtAddr> {}

/// Metadata for an AArch64 page table hierarchy.
///
/// `BITS` selects a virtual-address width from 25 through 52 bits. `PAGE_OFFSET_BITS` selects
/// the 4 KiB, 16 KiB, or 64 KiB granule, and `C` selects lower or upper coverage. The root may
/// contain fewer entries than a full lower-level table.
///
/// These parameters describe geometry without checking CPU features or configuring translation
/// control registers. The caller must pair the metadata with a compatible entry encoding and
/// hardware regime. In particular, wider address formats can require additional architectural
/// features.
pub struct AArch64PageTableMeta<
    const BITS: usize,
    const PAGE_OFFSET_BITS: usize,
    C: AArch64Coverage,
>(PhantomData<C>);

/// The minimum supported `T<n>SZ` value.
///
/// When `FEAT_LPA2` is supported, the minimal `T<n>SZ` value is 12, giving a
/// 52-bit virtual address space.
pub const TNSZ_MIN: usize = 12;
/// The maximum supported `T<n>SZ` value.
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

/// Derives level index widths from the address width and granule.
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

/// Describes AArch64 geometry with a potentially partial root table.
impl<const BITS: usize, const PAGE_OFFSET_BITS: usize, C> PageTableMeta
    for AArch64PageTableMeta<BITS, PAGE_OFFSET_BITS, C>
where
    C: AArch64Coverage + const PageTableCoverage<VirtAddr = VirtAddr>,
{
    /// The semantic virtual-address type used by AArch64 translation.
    type VirtAddr = VirtAddr;

    /// The number of index levels needed for the selected width and granule.
    ///
    /// Evaluation rejects widths outside the supported `T<n>SZ` interval.
    const LEVELS: usize = {
        if BITS > (ADDR_BITS - TNSZ_MIN) || BITS < (ADDR_BITS - TNSZ_MAX) {
            panic!("Unsupported number of bits for AArch64 4 KiB page table");
        }

        Self::PAGE_INDEX_BITS.div_ceil(Self::EACH_LEVEL_BITS)
    };

    /// The base-page offset width selected by the granule parameter.
    const PAGE_OFFSET_BITS: usize = PAGE_OFFSET_BITS;

    /// The index widths from the lowest level to the root.
    ///
    /// Only the root's width is reduced when the virtual width does not require a full table.
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

    /// The highest leaf level selected by this geometry.
    ///
    /// The implementation caps the level at two, or the root for shorter hierarchies. Callers
    /// must ensure that the hardware supports every leaf level used with this metadata.
    const MAX_PAGE_LEVEL: usize = {
        if Self::LEVELS < 3 {
            Self::LEVELS - 1
        } else {
            2
        }
    };
}

/// The local TLB flushing strategy for AArch64.
///
/// `Some(addr)` uses `TLBI VAAE1` for the address across ASIDs. `None` uses `TLBI VMALLE1` for
/// all stage-one EL1 translations in the current regime. Both operations are PE-local. Barriers
/// order preceding PTE stores and complete local invalidation before return.
///
/// Use this strategy only when that regime and local scope cover the affected translations.
/// See [`TlbFlusher`] for synchronization requirements.
pub struct AArch64LocalTlbFlusher;

/// Performs stage-one EL1 invalidation on the calling PE.
impl TlbFlusher<VirtAddr> for AArch64LocalTlbFlusher {
    /// Invalidates one address across ASIDs or all translations in the current regime.
    fn flush(addr: Option<VirtAddr>) {
        flush_local_tlb(addr);
    }
}

/// Invalidates one address or all stage-one translations on the current PE.
///
/// DSB orders prior PTE stores before TLBI and completes invalidation before subsequent access.
/// VAAE1 selects the VA's TTBR regime with VA bits 55:12 for every supported runtime granule.
fn flush_local_tlb(vaddr: Option<VirtAddr>) {
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
    //! Metadata checks for non-uniform AArch64 hierarchy geometry.

    use super::AArch64PageTableMeta64KiBLower;
    use crate::meta::PageTableMeta;

    /// Checks block sizes for a non-uniform 64 KiB page table hierarchy.
    ///
    /// A partially used root page table must not alter lower-level mapping sizes.
    #[test]
    fn computes_64k_level_page_sizes() {
        /// The 48-bit, lower-address format with 64 KiB base pages.
        type Meta = AArch64PageTableMeta64KiBLower<48>;

        assert_eq!(
            <Meta as PageTableMeta>::LEVEL_PAGE_SIZE,
            [1 << 16, 1 << 29, 1 << 42]
        );
    }
}
