//! Kernel allocator integrations.
//!
//! Groups the physical page allocator, Rust heap allocator, and vmalloc range
//! allocator used by the kernel.

pub mod malloc;
pub mod palloc;
pub mod vmalloc;
