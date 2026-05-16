//! Buddy allocator implementation.

/// Read-only summary of a managed section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedSection {
    /// Start PFN of the managed section.
    pub start: usize,
    /// Size of the managed section in pages.
    pub size: usize,
    /// Number of free pages in the managed section.
    pub free_pages: usize,
    /// Total number of pages in the managed section.
    pub total_pages: usize,
}

/// Usage statistics for the allocator.
pub struct AllocatorUsage {
    /// Total number of pages managed.
    pub total_pages: usize,
    /// Number of pages currently in use.
    pub used_pages: usize,
}

/// Buddy page allocator.
pub struct BuddyAllocator;
