//! Kernel allocator integrations.
//!
//! Groups the physical page allocator, Rust heap allocator, and vmalloc range
//! allocator used by the kernel.

#[cfg(not(feature = "host-test"))]
pub mod malloc;
pub mod palloc;
pub mod vmalloc;

#[cfg(feature = "host-test")]
pub mod malloc {
    pub fn init_malloc_current_cpu() {
        // Do nothing in host test
    }
}
