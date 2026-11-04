//! Common device-system bootstrap hooks.

use ecraldr_base::PlatformBootArg;

/// Discovers platform data, installs CPU topology, and maps controller MMIO.
///
/// Interrupt-controller setup is deferred until the architecture later-initialization hook.
pub fn discover_and_map_platform(arg: PlatformBootArg) {
    let description = exarch::platform::discover_platform(arg);
    let (cpu_topology, interrupt_controller, mmio_regions) = description.into_parts();
    crate::mp::init_cpu_list(cpu_topology);
    crate::mem::vmm::map_mmio_regions(&mmio_regions);
    interrupt_controller.publish()
}
