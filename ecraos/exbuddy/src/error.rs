//! Error types for buddy allocator operations.

/// The error type for buddy allocator operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuddyError {
    /// The provided page size is invalid.
    #[error("invalid page size")]
    InvalidPageSize,
    /// The provided alignment is invalid.
    #[error("invalid alignment")]
    InvalidAlignment,
    /// The provided page count is invalid.
    #[error("invalid page count")]
    InvalidPageCount,
    /// The provided order is invalid.
    #[error("invalid order")]
    InvalidOrder,
    /// The section is too small for buddy allocator.
    #[error("section too small")]
    SectionTooSmall,
    /// The section is too large for buddy allocator.
    #[error("section too large")]
    SectionTooLarge,
    /// The section overlaps with another section.
    #[error("section overlaps with another section")]
    SectionOverlap,
    /// No suitable memory found for operation.
    #[error("no suitable memory")]
    NoMemory,
    /// The specified address is not in any managed section.
    #[error("the specified address is not in any managed section")]
    NotInHeap,
    /// The specified address is not aligned.
    #[error("the specified address is not aligned")]
    NotAligned,
    /// The specified range (or part of it) is not allocated.
    #[error("(at least a part of) the specified range is not allocated")]
    NotAllocated,
    /// The specified range (or part of it) is already allocated.
    #[error("(at least a part of) the specified range is already allocated")]
    AlreadyAllocated,
    /// The specified order does not match the actual value.
    #[error("the specified order does not match the actual value")]
    OrderMismatch,
    /// The allocator is not initialized.
    #[error("allocator not initialized")]
    NotInitialized,
}

/// A specialized [`Result`] type for buddy allocator operations.
pub type BuddyResult<T = ()> = Result<T, BuddyError>;
