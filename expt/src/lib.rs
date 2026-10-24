#![doc = include_str!("../README.md")]
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
#![feature(doc_cfg)]
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

#[cfg(test)]
mod tests;
