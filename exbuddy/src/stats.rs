//! Aggregate allocator statistics.
//!
//! The statistics type tracks section capacity and current free-page usage.

use core::{iter::Sum, ops::Add};

/// Aggregate allocator statistics.
///
/// # About all values
///
/// - "Total pages" ([`total_pages()`](Self::total_pages)) is the total number
///   of all pages in all sections managed by the allocator.
/// - "Metadata pages" ([`meta_pages()`](Self::meta_pages)) is the total number
///   of pages used for metadata.
/// - "Heap pages" ([`heap_pages()`](Self::heap_pages)) is the total number of
///   allocatable pages, no matter if they are free or allocated.
/// - "Free pages" ([`free_pages()`](Self::free_pages)) is the total number of
///   allocatable and free pages.
/// - "Used pages" ([`used_pages()`](Self::used_pages)) is the total number of
///   allocatable and allocated pages.
/// - "Unused pages" ([`unused_pages()`](Self::unused_pages)) is the total
///   number of unused pages, which are outside both the metadata region and the
///   heap.
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct AllocatorStats {
    /// Total number of pages in the allocator.
    pub total_pages: usize,
    /// Number of pages used for metadata.
    pub meta_pages: usize,
    /// Number of pages used for the heap.
    pub heap_pages: usize,
    /// Number of free pages in the allocator.
    pub free_pages: usize,
}

/// Allocator-statistics accessors.
impl AllocatorStats {
    /// Returns the total number of pages in the allocator.
    pub fn total_pages(&self) -> usize {
        self.total_pages
    }

    /// Returns the number of pages used for metadata.
    pub fn meta_pages(&self) -> usize {
        self.meta_pages
    }

    /// Returns the number of pages used for the heap.
    pub fn heap_pages(&self) -> usize {
        self.heap_pages
    }

    /// Returns the number of free pages in the allocator.
    pub fn free_pages(&self) -> usize {
        self.free_pages
    }

    /// Returns the number of used pages in the allocator.
    pub fn used_pages(&self) -> usize {
        self.heap_pages - self.free_pages
    }

    /// Returns the number of unused pages in the allocator.
    ///
    /// Unused pages are outside both the metadata region and the heap.
    pub fn unused_pages(&self) -> usize {
        self.total_pages - self.meta_pages - self.heap_pages
    }
}

/// Adds allocator statistics field by field.
impl Add for AllocatorStats {
    /// The combined statistics type.
    type Output = Self;

    /// Adds two statistics values field by field.
    fn add(self, other: Self) -> Self {
        Self {
            total_pages: self.total_pages + other.total_pages,
            meta_pages: self.meta_pages + other.meta_pages,
            heap_pages: self.heap_pages + other.heap_pages,
            free_pages: self.free_pages + other.free_pages,
        }
    }
}

/// Sums allocator statistics field by field.
impl Sum for AllocatorStats {
    /// Sums all statistics values in the iterator.
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), Add::add)
    }
}
