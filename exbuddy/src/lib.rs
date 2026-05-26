//! Buddy page allocator.
//!
//! Provides a page-level memory allocator using the buddy system algorithm.

#![no_std]

/// Maximum buddy order.
///
/// With 4 KiB pages this gives 2^20 x 4 KiB = 4 GiB blocks.
pub const MAX_ORDER: usize = 20;

mod buddy;
pub mod error;
mod page_meta;
mod pfn;
mod section;
mod stats;

pub use buddy::BuddyAllocator;
pub use error::{BuddyError, BuddyResult};
pub use stats::AllocatorStats;
