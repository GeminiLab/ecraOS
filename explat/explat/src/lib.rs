//! Platform-Kernel interfaces.

#![no_std]

pub mod debug_console;
pub mod init;
pub mod power;

/// Re-export of the `crate_interface` crate.
pub mod crate_interface {
    pub use crate_interface::*;
}
