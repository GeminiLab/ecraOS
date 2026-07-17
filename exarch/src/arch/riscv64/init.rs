//! [`exarch::init::InitIf`] implementation for RISC-V.

use crate::init::{InitIf, PlatformBootArg};

/// The RISC-V implementation of the [`InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    /// Runs early platform initialization.
    fn init_early(_arg: PlatformBootArg) {}

    /// Runs later platform initialization.
    fn init_later(_arg: PlatformBootArg) {}

    /// Runs early application-processor initialization.
    fn init_early_ap() {}
}
