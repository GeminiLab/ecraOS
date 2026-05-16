//! Slab allocator with per-CPU caches and lock-free remote free.
//!
//! Provides small-object allocation (up to 2048 bytes) through per-CPU slab
//! caches backed by a generic [`PageProvider`] trait.

#![no_std]

extern crate alloc;

use memory_addr::VirtAddr;

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod size_class;
pub use size_class::SizeClass;

pub mod page;
pub mod cache;
pub mod slab;

/// Trait for slab to request/return pages from a backend allocator.
///
/// Returns [`VirtAddr`] because slab page headers and objects require virtual
/// addresses for pointer dereference.
pub trait PageProvider: Sync {
    /// Allocates `count` contiguous pages with `align` alignment.
    fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<VirtAddr>;

    /// Deallocates `count` pages starting at `addr`.
    fn dealloc_pages(&self, addr: VirtAddr, count: usize);
}
