//! Per-page metadata stored in the external metadata region.
//!
//! Each page frame in the heap has a corresponding [`PageMeta`] entry.
//! Free pages are linked together via intrusive doubly-linked lists using PFN indices.

use crate::pfn::SectionFrameNumber;

/// Sentinel value indicating "no page" in free-list links.
pub const PFN_NONE: SectionFrameNumber = SectionFrameNumber::from_u32(u32::MAX);

/// Page state flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageFlags {
    /// Page is free and sits in a buddy free list.
    Free = 0,
    /// Page is allocated. Only the head page of a buddy block is marked.
    Allocated = 1,
}

/// Metadata for a single page frame (12 bytes).
///
/// Head pages carry the `order` of the entire block.
/// Tail pages within a buddy block are marked `Allocated` (or `Slab`) with order 0.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageMeta {
    /// Current state of this page.
    pub flags: PageFlags,
    /// Order of the block (only meaningful on head pages).
    pub order: u8,
    /// Reserved padding.
    pub _pad: u16,
    /// Previous PFN in the same-order free list (`PFN_NONE` if head or not free).
    pub prev: SectionFrameNumber,
    /// Next PFN in the same-order free list (`PFN_NONE` if tail or not free).
    pub next: SectionFrameNumber,
}

/// Compile-time assertions to ensure `PageMeta` is exactly 12 bytes.
const _: () = assert!(core::mem::size_of::<PageMeta>() == 12);
/// Compile-time assertions to ensure `PageMeta` is 4-byte aligned.
const _: () = assert!(core::mem::align_of::<PageMeta>() == 4);

impl Default for PageMeta {
    fn default() -> Self {
        Self::new()
    }
}

impl PageMeta {
    /// Creates a zeroed (free, order-0) page meta.
    pub const fn new() -> Self {
        Self {
            flags: PageFlags::Free,
            order: 0,
            _pad: 0,
            prev: PFN_NONE,
            next: PFN_NONE,
        }
    }
}

// ---------------------------------------------------------------------------
// Free-list helpers operating on a `*mut PageMeta` array + head array
// ---------------------------------------------------------------------------

/// Encodes an optional section-local PFN for free-list storage.
///
/// Converts `None` to the free-list sentinel and real section-local PFNs to the
/// compact storage type used in page metadata.
#[inline]
pub fn encode_pfn(pfn: Option<SectionFrameNumber>) -> SectionFrameNumber {
    pfn.unwrap_or(PFN_NONE)
}

/// Decodes an optional section-local PFN from free-list storage.
///
/// Converts the free-list sentinel to `None` and real stored values to
/// section-local PFNs.
#[inline]
pub fn decode_pfn(value: SectionFrameNumber) -> Option<SectionFrameNumber> {
    (value != PFN_NONE).then_some(value)
}

/// Pushes `pfn` onto the front of `free_lists[order]`.
///
/// # Safety
///
/// `meta` must point to an array with at least `pfn + 1` entries.
/// `pfn` must not already be in any free list.
#[inline]
pub unsafe fn free_list_push(
    meta: &mut [PageMeta],
    free_lists: &mut [SectionFrameNumber],
    pfn: SectionFrameNumber,
    order: usize,
) {
    let old_head = free_lists[order];
    let m = &mut meta[pfn.as_usize()];
    m.prev = PFN_NONE;
    m.next = old_head;
    if let Some(old_head) = decode_pfn(old_head) {
        meta[old_head.as_usize()].prev = pfn;
    }
    free_lists[order] = encode_pfn(Some(pfn));
}

/// Removes `pfn` from the free list at `order`.
///
/// # Safety
///
/// `pfn` must currently be in `free_lists[order]`.
#[inline]
pub unsafe fn free_list_remove(
    meta: &mut [PageMeta],
    free_lists: &mut [SectionFrameNumber],
    pfn: SectionFrameNumber,
    order: usize,
) {
    let m = &mut meta[pfn.as_usize()];
    let prev = m.prev;
    let next = m.next;

    if let Some(prev) = decode_pfn(prev) {
        meta[prev.as_usize()].next = next;
    } else {
        // pfn was the head
        free_lists[order] = next;
    }
    if let Some(next) = decode_pfn(next) {
        meta[next.as_usize()].prev = prev;
    }

    let m = &mut meta[pfn.as_usize()];
    m.prev = PFN_NONE;
    m.next = PFN_NONE;
}
