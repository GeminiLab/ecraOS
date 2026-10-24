//! x86-64 page table hierarchy metadata and local TLB operations.
//!
//! The module describes standard four-level and five-level paging with 4 KiB
//! base pages and provides local TLB invalidation.

use memory_addr::VirtAddr;

use crate::{
    meta::{PageTableMeta, SymmetricCoverage},
    pt::cleanup::TlbFlusher,
};

/// Metadata for standard x86-64 four-level page table hierarchies.
///
/// The format addresses 48 virtual bits and supports leaf mappings up to 1 GiB
/// at level two.
pub struct X86Level4PageTableMeta;

/// Describes the four-level hierarchy's geometry and canonical coverage.
impl PageTableMeta for X86Level4PageTableMeta {
    /// The standard virtual-address type used by x86-64 paging.
    type VirtAddr = VirtAddr;

    /// The four paging levels in the 48-bit format.
    const LEVELS: usize = 4;
    /// The twelve offset bits in a 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each paging level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];
    /// The sign-extended lower and upper address ranges used by x86-64 paging.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The highest level that supports an x86-64 leaf mapping.
    ///
    /// Level two maps 1 GiB pages, while level three remains table-only.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Metadata for standard x86-64 five-level page table hierarchies.
///
/// The format addresses 57 virtual bits and supports leaf mappings up to 1 GiB
/// at level two.
pub struct X86Level5PageTableMeta;

/// Describes the five-level hierarchy's geometry and canonical coverage.
impl PageTableMeta for X86Level5PageTableMeta {
    /// The standard virtual-address type used by x86-64 paging.
    type VirtAddr = VirtAddr;

    /// The five paging levels in the 57-bit format.
    const LEVELS: usize = 5;
    /// The twelve offset bits in a 4 KiB page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The nine virtual-address bits consumed by each paging level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];
    /// The sign-extended lower and upper address ranges used by x86-64 paging.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The highest level that supports an x86-64 leaf mapping.
    ///
    /// Level two maps 1 GiB pages, while levels three and four remain table-only.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// The local TLB flushing strategy for x86-64.
///
/// `Some(addr)` uses `INVLPG` for the current address space, including global translations for
/// that address. `None` reloads CR3, invalidating non-global translations for the current PCID.
/// With CR4.PGE enabled, global translations survive the CR3 reload. Invalidation of other
/// PCIDs is not guaranteed.
///
/// Use this strategy only when affected translations belong to the current address space and
/// full invalidation does not need to discard global translations or translations for other
/// PCIDs. See [`TlbFlusher`] for synchronization requirements.
pub struct X86LocalTlbFlusher;

/// Performs invalidation in the calling CPU's current address space.
impl TlbFlusher<VirtAddr> for X86LocalTlbFlusher {
    /// Invalidates one mapping or reloads CR3 within [`X86LocalTlbFlusher`]'s scope.
    fn flush(addr: Option<VirtAddr>) {
        flush_local_tlb(addr);
    }
}

/// Invalidates translations in the current x86-64 address space on the calling CPU.
///
/// See [`X86LocalTlbFlusher`] for the scope of the address-specific and full operations.
#[inline]
pub fn flush_local_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        if let Some(vaddr) = vaddr {
            x86::tlb::flush(vaddr.into());
        } else {
            x86::tlb::flush_all();
        }
    }
}
