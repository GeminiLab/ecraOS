//! Error types for slab allocation operations.

use core::fmt;

/// The error type for slab allocation operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Invalid size, alignment, page size, or other input parameter.
    ///
    /// This is the only variant that `exslab` operations return. It is reported before any slab
    /// memory is accessed.
    InvalidParam,
    /// Not enough memory is available to satisfy the request.
    ///
    /// No `exslab` operation currently returns this variant. Missing slab memory is reported as
    /// [`crate::SlabAllocResult::NeedsSlab`].
    NoMemory,
    /// The slab allocator has not been initialized.
    ///
    /// No `exslab` operation currently returns this variant. Constructors return usable empty
    /// allocators and caches.
    NotInitialized,
}

/// Diagnostic formatting for [`AllocError`].
impl fmt::Display for AllocError {
    /// Formats the allocation error for diagnostic output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParam => write!(f, "invalid parameter"),
            Self::NoMemory => write!(f, "out of memory"),
            Self::NotInitialized => write!(f, "slab allocator not initialized"),
        }
    }
}

/// A [`Result`] alias with [`AllocError`] as the error type.
pub type AllocResult<T = ()> = Result<T, AllocError>;
