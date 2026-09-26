//! `x86_64` architecture-specific functions and types.
//!
//! The module describes standard four-level and five-level paging with 4 KiB
//! base pages and provides local TLB invalidation.

use memory_addr::VirtAddr;

use crate::PageTableMeta;

/// Flushes one mapping or the entire local x86-64 TLB.
///
/// [`Some`] invalidates the entry containing the supplied virtual address, while
/// [`None`] invalidates all local entries.
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
///
/// The format addresses 48 virtual bits and supports leaf mappings up to 1 GiB
/// at level two.
pub struct X86Level4PageTableMeta;

impl PageTableMeta for X86Level4PageTableMeta {
    /// The standard virtual-address type used by x86-64 paging.
    type VirtAddr = VirtAddr;

    /// The four paging levels in the 48-bit format.
    const LEVELS: usize = 4;
    /// The twelve offset bits in a 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each paging level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];

    /// The highest level that supports an x86-64 leaf mapping.
    ///
    /// Level two maps 1 GiB pages, while level three remains table-only.
    const MAX_PAGE_LEVEL: usize = 2;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// This delegates to the module's common x86-64 invalidation helper.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_x86_tlb(vaddr);
    }
}

/// Metadata for standard `x86_64` five-level page tables.
///
/// The format addresses 57 virtual bits and supports leaf mappings up to 1 GiB
/// at level two.
pub struct X86Level5PageTableMeta;

impl PageTableMeta for X86Level5PageTableMeta {
    /// The standard virtual-address type used by x86-64 paging.
    type VirtAddr = VirtAddr;

    /// The five paging levels in the 57-bit format.
    const LEVELS: usize = 5;
    /// The twelve offset bits in a 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each paging level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];

    /// The highest level that supports an x86-64 leaf mapping.
    ///
    /// Level two maps 1 GiB pages, while levels three and four remain table-only.
    const MAX_PAGE_LEVEL: usize = 2;

    /// Invalidates one mapping or all entries in the local TLB.
    ///
    /// This delegates to the module's common x86-64 invalidation helper.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        flush_x86_tlb(vaddr);
    }
}
