//! Architecture-specific functions and types.
//!
//! Each target module supplies [`crate::meta::PageTableMeta`] implementations and TLB
//! invalidation primitives for the page table hierarchy formats supported by that
//! architecture.

#[cfg(any(target_arch = "x86_64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "x86_64")))]
pub mod x86_64;

#[cfg(any(target_arch = "riscv64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "riscv64")))]
pub mod riscv64;

#[cfg(any(target_arch = "aarch64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "aarch64")))]
pub mod aarch64;
