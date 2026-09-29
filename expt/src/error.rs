//! Error and result types for page-table operations.

use crate::meta::VirtAddr;

/// An error produced by a page-table operation.
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
    /// existing mapping.
    #[error("The page is already mapped")]
    AlreadyMapped,
    /// The traversal encountered a huge-page mapping.
    ///
    /// This occurs when an operation needs to descend below a huge-page leaf
    /// without requesting that the mapping be split.
    #[error("The page is mapped to a huge page")]
    MappedToHugePage,
    /// A page-table allocation failed.
    ///
    /// The configured [`expalloc_trait::PageAllocator`] could not provide storage for a root or
    /// intermediate page table.
    #[error("Allocation failed")]
    AllocationFailed,
    /// The requested page-table level cannot contain leaf mappings.
    ///
    /// In other words, `level` is greater than
    /// [`crate::meta::PageTableMeta::MAX_PAGE_LEVEL`].
    #[error("The page cannot be a page at level {level}")]
    CannotBePage {
        /// The unsupported zero-indexed page-table level.
        ///
        /// Supported leaf levels range from zero through
        /// [`crate::meta::PageTableMeta::MAX_PAGE_LEVEL`].
        level: usize,
    },
    /// The virtual address is not canonical.
    ///
    /// This may also occur if a virtual address range includes non-canonical
    /// addresses. In such cases, the error will indicate the first
    /// non-canonical address encountered.
    #[error("The virtual address {vaddr:#x} is not canonical")]
    NonCanonical {
        /// The raw virtual address that is not canonical.
        vaddr: A,
    },
}

/// A result returned by page-table operations.
///
/// The default success value is `()`, and failures are represented by
/// [`PagingError`].
pub type PagingResult<A, T = ()> = Result<T, PagingError<A>>;
