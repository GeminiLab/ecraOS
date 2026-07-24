//! Slab allocator core.
//!
//! Provides small-object allocation up to 2048 bytes within caller-supplied
//! slab pages. The crate does not allocate pages, install mappings, provide
//! locking, or manage per-CPU state. Host kernels compose those policies around
//! [`SlabAllocator`].

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod size_class;
pub use size_class::SizeClass;

pub mod cache;
pub mod page;
pub mod slab;
pub use slab::{SlabAllocResult, SlabAllocator, SlabDeallocResult};

#[cfg(test)]
mod tests;
