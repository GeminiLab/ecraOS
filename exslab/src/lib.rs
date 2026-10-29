#![doc = include_str!("../README.md")]
#![no_std]

#[cfg(test)]
extern crate std;

pub mod cache;
pub mod error;
mod layout;
pub mod page;
pub mod size_class;
pub mod slab;

#[cfg(test)]
mod tests;

pub use error::{AllocError, AllocResult};
pub use layout::SlabLayout;
pub use size_class::SizeClass;
pub use slab::{SlabAllocResult, SlabAllocator, SlabDeallocResult};
