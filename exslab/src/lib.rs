#![doc = include_str!("../README.md")]
#![no_std]

#[cfg(test)]
extern crate std;

pub mod alloc;
pub mod cache;
pub mod error;
mod layout;
pub mod size_class;
pub mod slab_page;

#[cfg(test)]
mod tests;

pub use alloc::{SlabAllocResult, SlabAllocator, SlabDeallocResult};
pub use error::{AllocError, AllocResult};
pub use layout::SlabLayout;
pub use size_class::SizeClass;
