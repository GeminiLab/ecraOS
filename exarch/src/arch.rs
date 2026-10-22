//! Architecture-specific codes.

#[cfg(any(target_arch = "x86_64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "x86_64")))]
pub mod x86_64;

#[cfg(any(target_arch = "riscv64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "riscv64")))]
pub mod riscv64;

#[cfg(any(target_arch = "aarch64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "aarch64")))]
pub mod aarch64;

#[cfg(target_arch = "x86_64")]
#[doc(hidden)]
pub use x86_64 as current;

#[cfg(target_arch = "riscv64")]
#[doc(hidden)]
pub use riscv64 as current;

#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub use aarch64 as current;

/// RISC-V physical-target encoding on the host.
///
/// Privileged SBI calls remain target-only while the send contract is tested on every host.
#[cfg(all(test, not(target_arch = "riscv64")))]
#[path = "arch/riscv64/ipi/encoding.rs"]
mod riscv_ipi_encoding;

/// GICv3 redistributor and SGI encoding on the host.
///
/// Pure topology checks execute independently of EL1 registers and mapped controller memory.
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/gicv3/encoding.rs"]
mod gicv3_encoding;
