//! RISC-V 64 page-table metadata and TLB operations.
//!
//! The module provides the standard Sv39, Sv48, and Sv57 formats with 4 KiB base
//! pages.

use memory_addr::VirtAddr;

use crate::{PageTableCoverage, PageTableMeta};

/// Flushes one mapping or the entire local RISC-V TLB.
///
/// [`Some`] issues `SFENCE.VMA` for the supplied virtual address. [`None`]
/// issues the all-address form.
#[inline]
pub fn flush_riscv64_tlb(vaddr: Option<VirtAddr>) {
    if let Some(vaddr) = vaddr {
        riscv::asm::sfence_vma(0, vaddr.as_usize())
    } else {
        riscv::asm::sfence_vma_all();
    }
}

/// Metadata for the RISC-V Sv39 page-table format.
///
/// Sv39 uses three nine-bit levels and supports leaf mappings at every level.
pub struct Sv39PageTableMeta;

impl PageTableMeta for Sv39PageTableMeta {
    /// The standard virtual-address type used by Sv39.
    type VirtAddr = VirtAddr;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// This delegates to [`flush_riscv64_tlb`].
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    /// The three translation levels in Sv39.
    const LEVELS: usize = 3;
    /// The twelve offset bits in an Sv39 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv39 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9];
    /// Sv39 covers both the lower and upper portions of the virtual address space symmetrically.
    const COVERAGE: PageTableCoverage = PageTableCoverage::Symmetric;

    /// The highest level that supports an Sv39 leaf mapping.
    ///
    /// Every level in Sv39 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Metadata for the RISC-V Sv48 page-table format.
///
/// Sv48 uses four nine-bit levels and supports leaf mappings at every level.
pub struct Sv48PageTableMeta;

impl PageTableMeta for Sv48PageTableMeta {
    /// The standard virtual-address type used by Sv48.
    type VirtAddr = VirtAddr;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// This delegates to [`flush_riscv64_tlb`].
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    /// The four translation levels in Sv48.
    const LEVELS: usize = 4;
    /// The twelve offset bits in an Sv48 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv48 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];
    /// Sv48 covers both the lower and upper portions of the virtual address space symmetrically.
    const COVERAGE: PageTableCoverage = PageTableCoverage::Symmetric;

    /// The highest level that supports an Sv48 leaf mapping.
    ///
    /// Every level in Sv48 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 3;
}

/// Metadata for the RISC-V Sv57 page-table format.
///
/// Sv57 uses five nine-bit levels and supports leaf mappings at every level.
pub struct Sv57PageTableMeta;

impl PageTableMeta for Sv57PageTableMeta {
    /// The standard virtual-address type used by Sv57.
    type VirtAddr = VirtAddr;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// This delegates to [`flush_riscv64_tlb`].
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    /// The five translation levels in Sv57.
    const LEVELS: usize = 5;
    /// The twelve offset bits in an Sv57 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each Sv57 level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];
    /// Sv57 covers both the lower and upper portions of the virtual address space symmetrically.
    const COVERAGE: PageTableCoverage = PageTableCoverage::Symmetric;

    /// The highest level that supports an Sv57 leaf mapping.
    ///
    /// Every level in Sv57 may contain a leaf.
    const MAX_PAGE_LEVEL: usize = 4;
}
