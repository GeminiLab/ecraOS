//! [`exarch::init::InitIf`] implementation for RISC-V.

use crate::init::PlatformBootArg;

/// Runs early platform initialization.
pub fn init_early(arg: PlatformBootArg) {
    super::init_early_bsp(arg)
}

/// Runs later platform initialization.
pub fn init_later() {}

/// Runs early application-processor initialization.
pub fn init_early_ap() {
    super::init_early_ap();
}

pub fn init_later_ap() {}
