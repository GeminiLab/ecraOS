//! Buddy page allocator.
//!
//! Provides a page-level memory allocator using the buddy system algorithm.

#![no_std]
#![deny(stable_features)]
#![feature(likely_unlikely)]

#[cfg(test)]
extern crate std;

/// Maximum buddy order.
///
/// It determines the maximum size of buddy blocks - with 4 KiB pages this gives
/// 2^20 x 4 KiB = 4 GiB blocks. No single allocation can exceed this size.
/// Note that the buddy allocator can handle sections larger than this.
pub const MAX_ORDER: usize = 20;

mod buddy;
pub mod error;
mod page_meta;
pub mod pfn;
mod section;
mod stats;

#[cfg(test)]
mod tests;

#[doc(inline)]
pub use buddy::BuddyAllocator;
#[doc(inline)]
pub use error::{BuddyError, BuddyResult};
#[doc(inline)]
pub use section::BuddySection;
#[doc(inline)]
pub use stats::AllocatorStats;

/// Mark the current code path as unreachable with a internal error message.
macro_rules! internal_error {
    ($msg:literal $($args:tt)*) => {
        unreachable!(concat!("[Buddy Allocator Internal Error] ", $msg) $($args)*);
    };
}

pub(crate) use internal_error;

/// Converts a page count to the smallest order that can contain it.
///
/// Returns `Err(BuddyError::InvalidPageCount)` if the page count is too
/// large that even the largest order cannot contain it.
///
/// # Examples
///
/// ```
/// use exbuddy::{page_count_to_order_ceiling, MAX_ORDER, BuddyError};
///
/// assert_eq!(page_count_to_order_ceiling(0), Ok(0));
/// assert_eq!(page_count_to_order_ceiling(1), Ok(0));
/// assert_eq!(page_count_to_order_ceiling(2), Ok(1));
/// assert_eq!(page_count_to_order_ceiling(3), Ok(2));
/// assert_eq!(page_count_to_order_ceiling(4), Ok(2));
///
/// assert_eq!(page_count_to_order_ceiling(1usize << MAX_ORDER), Ok(MAX_ORDER));
/// assert_eq!(page_count_to_order_ceiling(1usize << MAX_ORDER + 1), Err(BuddyError::InvalidPageCount));
/// ```
#[inline]
pub const fn page_count_to_order_ceiling(page_count: usize) -> BuddyResult<usize> {
    if let Some(p) = page_count.checked_next_power_of_two() {
        let order = p.trailing_zeros() as usize;
        if order <= MAX_ORDER {
            return Ok(order);
        }
    }

    Err(BuddyError::InvalidPageCount)
}

/// Converts a page count to the largest order that can fit in it.
///
/// Returns `Err(BuddyError::InvalidPageCount)` if the page count is too
/// small that even the smallest order cannot fit in it (i.e. 0).
///
/// # Examples
///
/// ```
/// use exbuddy::{page_count_to_order_floor, MAX_ORDER, BuddyError, BuddyResult};
///
/// assert_eq!(page_count_to_order_floor(0), Err(BuddyError::InvalidPageCount));
/// assert_eq!(page_count_to_order_floor(1), Ok(0));
/// assert_eq!(page_count_to_order_floor(2), Ok(1));
/// assert_eq!(page_count_to_order_floor(3), Ok(1));
/// assert_eq!(page_count_to_order_floor(4), Ok(2));
///
/// assert_eq!(page_count_to_order_floor(1usize << MAX_ORDER), Ok(MAX_ORDER));
/// assert_eq!(page_count_to_order_floor(1usize << MAX_ORDER + 1), Ok(MAX_ORDER));
/// ```
#[inline]
pub const fn page_count_to_order_floor(page_count: usize) -> BuddyResult<usize> {
    if let Some(p) = page_count.highest_one() {
        let p = p as _;

        return Ok(if p > MAX_ORDER { MAX_ORDER } else { p });
    }

    Err(BuddyError::InvalidPageCount)
}
