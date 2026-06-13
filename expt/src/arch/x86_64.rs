/// `x86_64` architecture-specific functions and types.
use memory_addr::VirtAddr;

use crate::PageTableMeta;

#[inline]
fn flush_x86_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        if let Some(vaddr) = vaddr {
            x86::tlb::flush(vaddr.into());
        } else {
            x86::tlb::flush_all();
        }
    }
}

/// Metadata for standard `x86_64` four-level page tables.
pub struct X86Level4PageTableMeta;

impl PageTableMeta for X86Level4PageTableMeta {
    type VirtAddr = VirtAddr;

    const LEVELS: usize = 4;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];

    // Max page size 1GiB, at level 2 of levels 0-3.
    const MAX_PAGE_LEVEL: usize = 2;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_x86_tlb(vaddr);
    }
}

/// Metadata for standard `x86_64` five-level page tables.
pub struct X86Level5PageTableMeta;

impl PageTableMeta for X86Level5PageTableMeta {
    type VirtAddr = VirtAddr;

    const LEVELS: usize = 5;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];

    // Max page size 512GiB, at level 3 of levels 0-4.
    const MAX_PAGE_LEVEL: usize = 3;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_x86_tlb(vaddr);
    }
}
