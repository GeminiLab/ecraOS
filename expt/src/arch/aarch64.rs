//! AArch64 page-table metadata and TLB operations.
//!
//! The module describes the standard 4 KiB granule with four translation levels
//! and performs inner-shareable stage-one TLB invalidation.

use aarch64_cpu::asm::barrier::{ISH, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::PageTableMeta;

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
