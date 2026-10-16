//! Architecture-flexible multi-level page table management.
//!
//! This crate builds and mutates page table hierarchies described by
//! [`crate::meta::PageTableMeta`] and entries implementing [`crate::pte::GenericPTE`].
//! [`crate::pt::PageTable`] keeps the concrete representation available for compile-time dispatch,
//! while the [`opaque`] module offers runtime selection among page table hierarchy formats.
//!
//! Mutations are performed through [`crate::pt::PageTableCursor`]. See
//! [`crate::pt::PageTableCursorLike`] for range expansion, partial failure, and cleanup behavior.
//! [`crate::pt::cleanup::PendingCleanup`] groups pending invalidations and detached table-frame
//! allocations. The selected [`crate::pt::cleanup::TlbFlusher`] determines invalidation scope and
//! completion guarantees. Callers can transfer these records out of a cursor to coordinate
//! hardware synchronization before deallocation.
//!
//! Multi-level page table values deallocate all table frames in their page table hierarchies
//! when dropped. Detached table frames are deallocated by the cleanup batch. Mapped data frames
//! remain caller-managed. Callers must quiesce hardware users before dropping a multi-level page
//! table, and retain detached frames until required translation and page-walk synchronization has
//! completed. A mutable Rust borrow does not exclude hardware page walks.
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
//!
//! # Known issues
//!
//! RISC-V mapping flags are not validated. Leaf permissions must include `READ` or `EXECUTE`,
//! and `WRITE` requires `READ`. With debug assertions enabled, constructing a leaf without
//! `READ` or `EXECUTE` panics. With debug assertions disabled, such an entry above level zero can
//! be mistaken for a child-table descriptor. Traversal can then access mapped data as a page
//! table and deallocate data frames as table frames. Callers must supply valid RISC-V leaf
//! permissions until validation is implemented.

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
#![feature(gen_blocks)]
#![warn(missing_docs)]

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
pub mod pt;

/// Unit tests for concrete traversal, mapping, and TLB behavior.
///
/// The tests use compact and multi-frame formats with an in-memory allocator to
/// exercise page table hierarchy state deterministically.
#[cfg(test)]
mod tests;
