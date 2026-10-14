//! Architecture-flexible multi-level page table management.
//!
//! This crate builds and mutates page table hierarchies described by
//! [`crate::meta::PageTableMeta`] and entries implementing [`crate::pte::GenericPTE`].
//! [`crate::pt::PageTable`] keeps the concrete representation available for compile-time dispatch,
//! while the [`opaque`] module offers runtime selection among page table hierarchy formats.
//!
//! Mutations are performed through [`crate::pt::PageTableCursor`]. A cursor batches the TLB
//! invalidations required by mapping changes and performs local invalidations explicitly through
//! [`crate::pt::PageTableCursorLike::flush`] or automatically when it is dropped. Callers are
//! responsible for coordinating remote invalidation and concurrent hardware page walks.
//!
//! Multi-level page table values deallocate all table frames in their page table hierarchies
//! when dropped, including frames detached by mapping replacement. Mapped data frames remain
//! caller-managed. Callers must quiesce hardware users before dropping a multi-level page table.
//!
//! TODO: Fix the table-frame leak when mapping replacement detaches child page tables. The
//! current implementation loses their addresses and only deallocates frames reachable from the
//! roots during destruction, so it does not yet fulfill the deallocation contract above.
//!
//! # Terminology
//!
//! The terms **multi-level page table** and **multi-level page table structure** refer to the
//! Rust abstraction representing translation across all levels, such as [`crate::pt::PageTable`]
//! or [`crate::pt::DualPageTable`], and to values of those types. A **page table hierarchy** is the
//! corresponding in-memory data structure traversed by hardware. A single-root multi-level
//! page table represents one hierarchy, while a dual-root value represents two independent
//! hierarchies. A **page table** is one table at a single level within a hierarchy.
//! **Table frames**, also called **page table frames**, are the physical frames storing those
//! tables. One page table may occupy one or more table frames.

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
    //! Page table entry types and flags.
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
/// The tests use compact and multi-frame formats with an in-memory allocator to
/// exercise page table hierarchy state deterministically.
#[cfg(test)]
mod tests;
