use core::{iter::Sum, ops::Add};

#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct AllocatorStats {
    pub total_pages: usize,
    pub meta_pages: usize,
    pub heap_pages: usize,
    pub free_pages: usize,
}

impl AllocatorStats {
    pub fn total_pages(&self) -> usize {
        self.total_pages
    }

    pub fn meta_pages(&self) -> usize {
        self.meta_pages
    }

    pub fn heap_pages(&self) -> usize {
        self.heap_pages
    }

    pub fn free_pages(&self) -> usize {
        self.free_pages
    }

    pub fn used_pages(&self) -> usize {
        self.heap_pages - self.free_pages
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
