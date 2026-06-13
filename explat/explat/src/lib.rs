//! Platform-Kernel interfaces.

#![no_std]

pub mod debug_console;
pub mod init;
pub mod power;

/// Re-export of crates that any `explat` implementation must depend on.
pub mod reexport {
    /// Re-export of the `crate_interface` crate.
    pub mod crate_interface {
        pub use crate_interface::*;
    }

    /// Re-export of the `memory_addr` crate.
    pub mod memery_addr {
        pub use memory_addr::*;
    }
}
