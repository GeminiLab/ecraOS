//! Common device-system bootstrap hooks.

use ecraldr_base::PlatformBootArg;
use exarch::mem::MemoryRegionFlags;

// TODO: remove this duplicate definition
/// Access and memory-type bits in memory region flags that can affect the flag bits of page table
/// entries.
const PAGE_TABLE_BITS: MemoryRegionFlags = MemoryRegionFlags::READ
    .union(MemoryRegionFlags::WRITE)
    .union(MemoryRegionFlags::EXECUTE)
    .union(MemoryRegionFlags::DEVICE)
    .union(MemoryRegionFlags::UNCACHED);

/// Discovers platform data, installs CPU topology, and maps controller MMIO.
///
/// Interrupt-controller setup is deferred until the architecture later-initialization hook.
pub fn discover_and_map_platform(arg: PlatformBootArg) {
    let description = exarch::platform::discover_platform(arg);
    let (cpu_topology, interrupt_controller, mmio_regions) = description.into_parts();
    crate::mp::init_cpu_list(cpu_topology);
    crate::mem::pmm::add_and_map_phys_regions(&mmio_regions, |region, old| {
        old.flags.contains(MemoryRegionFlags::RESERVED)
            && (!old.flags.intersects(PAGE_TABLE_BITS)
                || old.flags & PAGE_TABLE_BITS == region.flags & PAGE_TABLE_BITS)
    });
    interrupt_controller.publish()
}
