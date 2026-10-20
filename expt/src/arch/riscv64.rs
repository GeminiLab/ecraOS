//! RISC-V 64 page table hierarchy metadata and TLB operations.
//!
//! The module provides the standard Sv39, Sv48, and Sv57 formats with 4 KiB base
//! pages.

use memory_addr::VirtAddr;

use crate::{
    meta::{PageTableMeta, SymmetricCoverage},
    pt::cleanup::TlbFlusher,
};

/// Metadata for the RISC-V Sv39 page table hierarchy format.
///
/// Sv39 uses three nine-bit levels and supports leaf mappings at every level.
pub struct Sv39PageTableMeta;

impl PageTableMeta for Sv39PageTableMeta {
    /// The standard virtual-address type used by Sv39.
    type VirtAddr = VirtAddr;

    /// The three translation levels in Sv39.
    const LEVELS: usize = 3;
    /// The twelve offset bits in an Sv39 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv39 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9];
    /// Sv39 uses sign-extended lower and upper address ranges.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The highest level that supports an Sv39 leaf mapping.
    ///
    /// Every level in Sv39 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Metadata for the RISC-V Sv48 page table hierarchy format.
///
/// Sv48 uses four nine-bit levels and supports leaf mappings at every level.
pub struct Sv48PageTableMeta;

impl PageTableMeta for Sv48PageTableMeta {
    /// The standard virtual-address type used by Sv48.
    type VirtAddr = VirtAddr;

    /// The four translation levels in Sv48.
    const LEVELS: usize = 4;
    /// The twelve offset bits in an Sv48 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv48 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];
    /// Sv48 uses sign-extended lower and upper address ranges.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The highest level that supports an Sv48 leaf mapping.
    ///
    /// Every level in Sv48 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 3;
}

/// Metadata for the RISC-V Sv57 page table hierarchy format.
///
/// Sv57 uses five nine-bit levels and supports leaf mappings at every level.
pub struct Sv57PageTableMeta;

impl PageTableMeta for Sv57PageTableMeta {
    /// The standard virtual-address type used by Sv57.
    type VirtAddr = VirtAddr;

    /// The five translation levels in Sv57.
    const LEVELS: usize = 5;
    /// The twelve offset bits in an Sv57 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv57 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];
    /// Sv57 uses sign-extended lower and upper address ranges.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The highest level that supports an Sv57 leaf mapping.
    ///
    /// Every level in Sv57 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 4;
}

/// The local TLB flushing strategy for RISC-V 64.
///
/// `Some(addr)` uses address-specific `SFENCE.VMA` with ASID zero in a general-purpose register
/// as `rs2`. It invalidates non-global leaf translations for that address and ASID zero.
/// `None` uses `SFENCE.VMA x0, x0`, covering all addresses and ASIDs, including global
/// translations.
///
/// Use this strategy only when address-specific invalidations affect ASID-zero, non-global
/// translations. A global ancestor descriptor also makes a translation global. See
/// [`TlbFlusher`] for synchronization requirements.
pub struct Riscv64LocalTlbFlusher;

impl TlbFlusher<VirtAddr> for Riscv64LocalTlbFlusher {
    fn flush(addr: Option<VirtAddr>) {
        flush_local_tlb(addr);
    }
}

/// Flushes one mapping or the entire local RISC-V TLB.
///
/// See [`Riscv64LocalTlbFlusher`] for the ASID and global-translation scope of each operation.
#[inline]
fn flush_local_tlb(vaddr: Option<VirtAddr>) {
    if let Some(vaddr) = vaddr {
        riscv::asm::sfence_vma(0, vaddr.as_usize())
    } else {
        riscv::asm::sfence_vma_all();
    }
}
