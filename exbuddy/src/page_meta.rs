//! Per-page metadata stored in the external metadata region.
//!
//! Each page frame in the heap has a corresponding [`PageMeta`] entry.
//! Free pages are linked together via intrusive doubly-linked lists using PFN indices.

use crate::pfn::{OptionSectionFrameNumber, SectionFrameNumber};

/// Page state flags.
///
/// It can be considered to have the following bitfield layout:
/// - Bit 0: **Head**. Whether the page is the head of a buddy block.
/// - Bit 1: **Allocated**. Whether the block is allocated.
///
/// It's designed such that zero-initialization is safe for pages that are not block heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PageFlags {
    /// Page is inside a larger buddy block.
    #[default]
    InBlock = 0,
    /// Page is the head of a free block.
    Free = 2,
    /// Page is the head of an allocated block.
    Allocated = 3,
}

/// Metadata for a single page frame.
///
/// This struct should be 12 bytes in size and 4-byte aligned.
///
/// For free or allocated blocks, only the head page carries the `flags` and
/// `order` of the whole block, while other pages are marked as `InBlock`.
///
/// It's designed such that zero-initialization is safe for pages that are not
/// block heads.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageMeta {
    /// Current state of this page.
    pub flags: PageFlags,
    /// Order of the block.
    ///
    /// This field is valid only if `flags` does not equal `PageFlags::InBlock`.
    pub order: u8,
    /// Reserved padding.
    pub _pad: u16,
    /// Previous PFN in the same-order free list.
    ///
    /// This field is valid only if `flags` equals `PageFlags::Free`.
    pub prev: OptionSectionFrameNumber,
    /// Next PFN in the same-order free list.
    ///
    /// This field is valid only if `flags` equals `PageFlags::Free`.
    pub next: OptionSectionFrameNumber,
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
            prev: OptionSectionFrameNumber::NONE,
            next: OptionSectionFrameNumber::NONE,
        }
    }

    /// Reads the [`SectionFrameNumber`] of the previous page in the same-order
    /// free list.
    ///
    /// The value is valid only if `flags` equals `PageFlags::Free`.
    #[inline]
    pub const fn prev(&self) -> Option<SectionFrameNumber> {
        self.prev.into_option_sfn()
    }

    /// Reads the [`SectionFrameNumber`] of the next page in the same-order
    /// free list.
    ///
    /// The value is valid only if `flags` equals `PageFlags::Free`.
    #[inline]
    pub const fn next(&self) -> Option<SectionFrameNumber> {
        self.next.into_option_sfn()
    }

    /// Sets the previous page in the same-order free list to the given
    /// [`SectionFrameNumber`] or `None`.
    #[inline]
    pub const fn set_prev(&mut self, sfn: Option<SectionFrameNumber>) {
        self.prev = OptionSectionFrameNumber::from_option_sfn(sfn);
    }

    /// Sets the next page in the same-order free list to the given
    /// [`SectionFrameNumber`] or `None`.
    #[inline]
    pub const fn set_next(&mut self, sfn: Option<SectionFrameNumber>) {
        self.next = OptionSectionFrameNumber::from_option_sfn(sfn);
    }

    /// Sets the previous page in the same-order free list to the given
    /// [`SectionFrameNumber`].
    #[inline]
    pub const fn set_prev_to(&mut self, sfn: SectionFrameNumber) {
        self.set_prev(Some(sfn));
    }

    /// Sets the next page in the same-order free list to the given
    /// [`SectionFrameNumber`].
    #[inline]
    pub const fn set_next_to(&mut self, sfn: SectionFrameNumber) {
        self.set_next(Some(sfn));
    }

    /// Sets the previous page in the same-order free list to `None`.
    #[inline]
    pub const fn clear_prev(&mut self) {
        self.set_prev(None);
    }

    /// Sets the next page in the same-order free list to `None`.
    #[inline]
    pub const fn clear_next(&mut self) {
        self.set_next(None);
    }
}
