use memory_addr::VirtAddr;

use crate::PageTableMeta;

#[inline]
pub fn flush_riscv64_tlb(vaddr: Option<VirtAddr>) {
    if let Some(vaddr) = vaddr {
        riscv::asm::sfence_vma(0, vaddr.as_usize())
    } else {
        riscv::asm::sfence_vma_all();
    }
}

pub struct Sv39PageTableMeta;

impl PageTableMeta for Sv39PageTableMeta {
    type VirtAddr = VirtAddr;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    const LEVELS: usize = 3;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9];

    const MAX_PAGE_LEVEL: usize = 2;
}

pub struct Sv48PageTableMeta;

impl PageTableMeta for Sv48PageTableMeta {
    type VirtAddr = VirtAddr;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    const LEVELS: usize = 4;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];

    const MAX_PAGE_LEVEL: usize = 3;
}

pub struct Sv57PageTableMeta;

impl PageTableMeta for Sv57PageTableMeta {
    type VirtAddr = VirtAddr;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_riscv64_tlb(vaddr);
    }

    const LEVELS: usize = 5;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];

    const MAX_PAGE_LEVEL: usize = 4;
}
