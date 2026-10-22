#[cfg(any(target_arch = "x86_64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "x86_64")))]
pub mod x86_64;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", ecraos_doc))]
#[cfg_attr(
    ecraos_doc,
    doc(cfg(any(target_arch = "riscv32", target_arch = "riscv64")))
)]
pub mod riscv;

#[cfg(any(target_arch = "aarch64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "aarch64")))]
pub mod aarch64;

#[cfg(any(target_arch = "arm", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "arm")))]
pub mod arm;

#[cfg(any(target_arch = "loongarch64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "loongarch64")))]
pub mod loongarch64;
