//! Error types for slab allocation operations.

use core::fmt;

/// The error type for slab allocation operations.
///
/// Each variant describes a specific failure mode of the slab allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Invalid size, alignment, or other input parameter.
    InvalidParam,
    /// Not enough memory is available to satisfy the request.
    NoMemory,
    /// The slab allocator has not been initialized.
    NotInitialized,
}

impl fmt::Display for AllocError {
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
