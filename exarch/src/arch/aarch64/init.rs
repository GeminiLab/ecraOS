//! AArch64 implementation of the common platform initialization contract.

use crate::init::PlatformBootArg;

/// Runs bootstrap-CPU early initialization.
pub fn init_early(arg: PlatformBootArg) {
    super::init_early_bsp(arg);
}

/// Runs late bootstrap-CPU initialization.
pub fn init_later() {}

/// Runs early application-CPU initialization.
pub fn init_early_ap() {
    super::init_early_ap();
}

/// Runs late application-CPU initialization.
pub fn init_later_ap() {
    super::trap::init_percpu();
}
