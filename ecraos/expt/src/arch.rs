//! Architecture-specific functions and types.
//!
//! Each target module supplies [`crate::meta::PageTableMeta`] implementations and TLB
//! invalidation primitives for the page-table formats supported by that
//! architecture.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

/// The target-selected PE-local invalidation primitive.
///
/// Remote IPI handlers need no runtime page-table metadata type to execute this function.
#[cfg(target_arch = "aarch64")]
pub use aarch64::flush_local_tlb;
#[cfg(target_arch = "riscv64")]
pub use riscv64::flush_riscv64_tlb as flush_local_tlb;
#[cfg(target_arch = "x86_64")]
pub use x86_64::flush_local_tlb;
