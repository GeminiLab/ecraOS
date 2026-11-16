//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`ecraldr_base`]
//! and `ecraldr`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![feature(extern_item_impls)]
#![feature(doc_cfg)]
#![deny(unfulfilled_lint_expectations)]

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

pub use expanic::{bug, kpanic};

mod boot;
mod device;
#[cfg(any(feature = "foundation-test", doc))]
pub mod foundation_test;
mod kernel_if;
pub mod logging;
pub mod mem;
pub mod mp;
pub mod panic;
mod percpu;
pub mod task;
mod test_guard;
mod timer;
mod trap;

pub use boot::kernel_entry_ap;

// Temporary re-export, visibility of `trap::ipi` to be reviewed later.
pub use trap::ipi::IpiError;

#[doc(hidden)]
pub mod reexport {
    pub use exarch;
}

#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {
        crate::reexport::exarch::dbcn_println!($($arg)*)
    };
}

#[eii]
pub fn app_main_bsp() {}

#[eii]
pub fn app_main_ap() {
    task::run_idle()
}

/// Calls the BSP application entry point.
///
/// This trampoline function exists because testing with the current toolchain has only shown
/// reliable symbol generation when the `#[eii]` declaration and its caller are in the same module.
#[inline(never)]
pub(crate) fn run_app_main_bsp() {
    app_main_bsp();
}

/// Calls the AP application entry point.
///
/// This trampoline function exists because testing with the current toolchain has only shown
/// reliable symbol generation when the `#[eii]` declaration and its caller are in the same module.
#[inline(never)]
pub(crate) fn run_app_main_ap() {
    app_main_ap();
}
