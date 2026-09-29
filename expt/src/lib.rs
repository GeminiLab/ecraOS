//! Architecture-flexible page-table management.
//!
//! This crate builds and mutates hierarchical page tables described by
//! [`crate::meta::PageTableMeta`] and entries implementing [`crate::pte::GenericPTE`].
//! [`crate::pt::PageTable`] keeps the concrete representation available for compile-time dispatch,
//! while the [`opaque`] module offers runtime selection among page-table formats.
//!
//! Mutations are performed through [`crate::pt::PageTableCursor`]. A cursor batches the TLB
//! invalidations required by mapping changes and performs them explicitly through
//! [`crate::pt::PageTableCursorLike::flush`] or automatically when it is dropped.

#![no_std]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(generic_const_items)]
#![feature(const_trait_impl)]
#![feature(const_ops)]
#![feature(const_try)]
#![feature(const_convert)]
#![feature(const_closures)]
#![feature(const_result_trait_fn)]

/// The standard library used by unit tests.
///
/// Production builds remain `no_std`, while the test module uses host allocation
/// and thread-local storage.
#[cfg(test)]
extern crate std;

pub mod arch;
pub mod meta;
pub mod opaque;
pub mod pte {
    //! Page-table entry types and flags.
    //!
    //! This module re-exports the entry abstractions supplied by the
    //! [`page_table_entry`] crate.
    pub use page_table_entry::*;
}
pub mod error;
mod flush;
pub mod pt;

/// Unit tests for concrete traversal, mapping, and TLB behavior.
///
/// The tests use a compact three-level format and an in-memory frame allocator
/// to exercise page-table state deterministically.
#[cfg(test)]
mod tests;
