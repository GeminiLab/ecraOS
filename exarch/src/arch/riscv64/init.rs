//! [`exarch::init::InitIf`] implementation for RISC-V.

use crate::init::{InitIf, PlatformBootArg};

/// The RISC-V implementation of the [`InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    /// Runs early platform initialization.
    fn init_early(arg: PlatformBootArg) {
        super::init_early_bsp(arg)
    }

    /// Runs later platform initialization.
    fn init_later() {}

    /// Runs early application-processor initialization.
    fn init_early_ap() {
        super::init_early_ap();
    }

    fn init_later_ap() {}
}
