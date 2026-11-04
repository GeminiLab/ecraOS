//! Platform initialization functions for RISC-V.

use crate::init::PlatformBootArg;

/// Runs early platform initialization.
pub fn init_early(arg: PlatformBootArg) {
    super::init_early_bsp(arg)
}

/// Initializes the PLIC and the BSP-local interrupt sources.
///
/// Consumes the controller configuration published after discovery and MMIO mapping.
///
/// # Panics
///
/// Panics if platform discovery did not publish the controller configuration.
pub fn init_later() {
    let config = crate::platform::take_interrupt_controller_config();
    super::trap::init_external_controller(config);
    super::trap::init_percpu();
}

/// Runs early application-processor initialization.
pub fn init_early_ap() {
    super::init_early_ap();
}

/// Initializes the AP's PLIC context and local interrupt sources.
pub fn init_later_ap() {
    super::trap::init_percpu();
}
