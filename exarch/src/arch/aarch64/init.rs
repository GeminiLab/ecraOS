//! AArch64 implementation of the common platform initialization contract.

use cfg_or_dummy::cfg_or_dummy;

use crate::init::PlatformBootArg;

/// Runs bootstrap-CPU early initialization.
pub fn init_early(arg: PlatformBootArg) {
    super::init_early_bsp(arg);
}

/// Initializes the BSP global controller and its local interrupt interface.
///
/// Consumes the controller configuration published after discovery and MMIO mapping.
///
/// # Panics
///
/// Panics if platform discovery did not publish the controller configuration.
#[cfg_or_dummy(target_arch = "aarch64")]
pub fn init_later() {
    let config = crate::platform::take_interrupt_controller_config();
    super::trap::init_external_controller(config);
    super::trap::init_percpu();
}

/// Runs early application-CPU initialization.
pub fn init_early_ap() {
    super::init_early_ap();
}

/// Runs late application-CPU initialization.
pub fn init_later_ap() {
    super::trap::init_percpu();
}
