//! Error and result types for page table hierarchy operations.
//!
//! These types report traversal, table-frame allocation, and virtual-address validation failures.

use crate::meta::VirtAddr;

/// An error produced by a page table hierarchy operation.
///
/// Errors describe unavailable mappings, unsupported traversal, allocation failure, and invalid
/// virtual-address coverage. A failed mutation can retain earlier changes and pending cleanup, as
/// described by [`PageTableCursorLike`](crate::pt::PageTableCursorLike).
#[derive(Debug, thiserror::Error)]
pub enum PagingError<A: VirtAddr> {
    /// The requested virtual page has no mapping.
    ///
    /// This occurs when an operation requires an existing leaf or intermediate
    /// table entry but finds an unused entry.
    #[error("The page is not mapped")]
    NotMapped,
    /// The requested virtual page already has a mapping.
    ///
    /// This variant is available for operations that reject replacement of an
    /// existing mapping. Current cursor `map` operations replace existing mappings and do not
    /// produce this variant.
    #[error("The page is already mapped")]
    AlreadyMapped,
    /// The traversal encountered a huge-page mapping.
    ///
    /// This occurs when an operation needs to descend below a huge-page leaf
    /// without requesting that the mapping be split.
    #[error("The page is mapped to a huge page")]
    MappedToHugePage,
    /// An allocation of table frames failed.
    ///
    /// The configured [`expalloc_trait::PageAllocator`] could not provide table
    /// frames for a root or intermediate page table.
    #[error("Allocation failed")]
    AllocationFailed,
    /// The requested page table level cannot contain leaf mappings.
    ///
    /// In other words, `level` is greater than
    /// [`crate::meta::PageTableMeta::MAX_PAGE_LEVEL`].
    #[error("The page cannot be a page at level {level}")]
    CannotBePage {
        /// The unsupported zero-indexed page table level.
        ///
        /// Supported leaf levels range from zero through
        /// [`crate::meta::PageTableMeta::MAX_PAGE_LEVEL`].
        level: usize,
    },
    /// An invalid virtual address or range.
    ///
    /// This reports an address outside the selected coverage or a range crossing a canonical
    /// hole. It also reports overflow while computing or aligning a range endpoint. In the
    /// overflow case, the reported address may itself be canonical.
    #[error("The virtual address {vaddr:#x} is not canonical")]
    NonCanonical {
        /// The virtual address associated with the validation failure.
        ///
        /// This is the address rejected by coverage validation, or the address associated
        /// with an overflowing range calculation.
        vaddr: A,
    },
}

/// A result returned by page table hierarchy operations.
///
/// The default success value is `()`, and failures are represented by
/// [`PagingError`].
pub type PagingResult<A, T = ()> = Result<T, PagingError<A>>;
