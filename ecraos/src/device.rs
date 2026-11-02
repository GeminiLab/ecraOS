//! Common device-system bootstrap hooks.

use ecraldr_base::PlatformBootArg;

/// Installs firmware-discovered platform data into the common kernel state.
///
/// Architecture-specific firmware parsing and interrupt-controller configuration live in
/// `exarch::platform`. This module only consumes the resulting CPU topology and initializes the
/// architecture-owned controller driver before ordinary device enumeration begins.
pub fn init_platform(arg: PlatformBootArg) {
    let description = exarch::platform::discover(arg);
    let (cpu_topology, interrupt_controller) = description.into_parts();
    crate::mp::init_cpu_list(cpu_topology);
    interrupt_controller.initialize();
}
