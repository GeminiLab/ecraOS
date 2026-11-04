//! Platform initialization functions for x86_64.

use crate::init::PlatformBootArg;

/// Runs bootstrap-CPU early initialization.
pub fn init_early(_arg: PlatformBootArg) {
    super::init_early();
}

/// Initializes the BSP global and local interrupt controllers from published platform data.
///
/// # Panics
///
/// Panics if platform discovery did not publish the controller configuration.
pub fn init_later() {
    super::init_later();
}

/// Installs early CPU-local exception state on an AP.
pub fn init_early_ap() {
    super::init_early_ap();
}

/// Initializes the AP-local interrupt controller.
pub fn init_later_ap() {
    super::init_later_ap();
}
