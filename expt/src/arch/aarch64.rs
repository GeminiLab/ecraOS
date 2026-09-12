use aarch64_cpu::asm::barrier::{ISH, SY, dsb, isb};
use memory_addr::VirtAddr;

use crate::PageTableMeta;

/// Metadata for AArch64 4 KiB, four-level, 48-bit translation tables.
pub struct Aarch64PageTableMeta;

impl PageTableMeta for Aarch64PageTableMeta {
    type VirtAddr = VirtAddr;

    const LEVELS: usize = 4;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9; Self::LEVELS];
    const MAX_PAGE_LEVEL: usize = 2;

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
