//! Buddy page allocator.
//!
//! Provides a page-level memory allocator using the buddy system algorithm.

#![no_std]

pub mod error;
pub use error::{AllocError, AllocResult};

pub mod page_meta;
pub use page_meta::{PageFlags, PageMeta, PFN_NONE};

pub mod buddy;
pub use buddy::{AllocatorUsage, BuddyAllocator, ManagedSection};
