//! Error and result types for page-table operations.

/// An error produced by a page-table operation.
#[derive(Debug, thiserror::Error)]
pub enum PagingError {
    /// The requested virtual page has no mapping.
    ///
    /// This occurs when an operation requires an existing leaf or intermediate
    /// table entry but finds an unused entry.
    #[error("The page is not mapped")]
    NotMapped,
    /// The requested virtual page already has a mapping.
    ///
    /// This variant is used for operations that reject replacement of an
    /// existing mapping.
    #[error("The page is already mapped")]
    AlreadyMapped,
    /// The traversal encountered a huge-page mapping.
    ///
    /// This occurs when an operation needs to descend below a huge-page leaf
    /// while not requested that the mapping be split.
    #[error("The page is mapped to a huge page")]
    MappedToHugePage,
    /// A page-table allocation failed.
    ///
    /// The configured [`PageAllocator`] could not provide storage for a root or
    /// intermediate page table.
    #[error("Allocation failed")]
    AllocationFailed,
    /// The requested page-table level cannot contain leaf mappings.
    ///
    /// Id est, `level` is greater than [`PageTableMeta::MAX_PAGE_LEVEL`].
    #[error("The page cannot be a page at level {level}")]
    CannotBePage {
        /// The unsupported zero-indexed page-table level.
        ///
        /// Supported leaf levels range from zero through
        /// [`PageTableMeta::MAX_PAGE_LEVEL`].
        level: usize,
    },
}

/// A result returned by page-table operations.
///
/// The default success value is `()`, and failures are represented by
/// [`PagingError`].
pub type PagingResult<T = ()> = Result<T, PagingError>;
