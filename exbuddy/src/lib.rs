//! Buddy page allocator.
//!
//! Provides a page-level memory allocator using the buddy system algorithm.

#![no_std]

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod page_meta;
pub use page_meta::{PFN_NONE, PageFlags, PageMeta};

pub mod buddy;
pub use buddy::{AllocatorUsage, BuddyAllocator, MAX_ORDER, ManagedSection};
