use core::{iter::Sum, ops::Add};

/// Statistics of the allocator.
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

    /// Returns the number of unused pages in the allocator, which is pages that
    /// are not used for metadata or the heap.
    pub fn unused_pages(&self) -> usize {
        self.total_pages - self.meta_pages - self.heap_pages
    }
}

impl Add for AllocatorStats {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            total_pages: self.total_pages + other.total_pages,
            meta_pages: self.meta_pages + other.meta_pages,
            heap_pages: self.heap_pages + other.heap_pages,
            free_pages: self.free_pages + other.free_pages,
        }
    }
}

impl Sum for AllocatorStats {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), Add::add)
    }
}
