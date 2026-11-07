//! Firmware-backed platform discovery.

use alloc::{boxed::Box, vec::Vec};

use ecraldr_base::PlatformBootArg;
use exspin::SpinNoIrq;
use lazyinit::LazyInit;
use look_at::look_at;

use crate::{mem::PhysMemoryRegion, power::PhysicalCpuId, trap::irq::ArchIrqConfig};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", ecraos_doc, test))]
pub mod dt;

/// The firmware-discovered physical CPU topology.
///
/// The architecture-specific platform parser owns the interpretation of CPU identifiers. The
/// common kernel consumes the identifiers when it builds its logical CPU inventory.
// TODO: make this struct the real topology record, not merely the CPU list
pub struct CpuTopology {
    /// The physical identifiers of all CPUs available to the kernel.
    cpu_ids: Box<[PhysicalCpuId]>,
    /// The physical identifier of the bootstrap CPU.
    bsp_id: PhysicalCpuId,
}

impl CpuTopology {
    /// Creates a CPU topology from firmware-discovered identifiers.
    ///
    /// The list must be non-empty and must contain the bootstrap identifier. Duplicate identifiers
    /// are rejected because they cannot be represented by the kernel's physical-to-logical map.
    pub(crate) fn new(cpu_ids: Box<[PhysicalCpuId]>, bsp_id: PhysicalCpuId) -> Self {
        assert!(!cpu_ids.is_empty(), "platform discovered no CPUs");
        assert!(
            cpu_ids.contains(&bsp_id),
            "platform CPU list does not contain the bootstrap CPU"
        );
        for (index, cpu_id) in cpu_ids.iter().enumerate() {
            assert!(
                !cpu_ids[..index].contains(cpu_id),
                "platform discovered duplicate CPU identifier {cpu_id:#x}"
            );
        }
        Self { cpu_ids, bsp_id }
    }

    /// Returns the discovered physical CPU identifiers in firmware order.
    pub fn cpu_ids(&self) -> &[PhysicalCpuId] {
        &self.cpu_ids
    }

    /// Returns the physical identifier of the bootstrap CPU.
    pub const fn bsp_id(&self) -> PhysicalCpuId {
        self.bsp_id
    }

    /// Transfers the discovered CPU identifiers and bootstrap identifier to the kernel CPU manager.
    pub fn into_parts(self) -> (Box<[PhysicalCpuId]>, PhysicalCpuId) {
        (self.cpu_ids, self.bsp_id)
    }
}

/// Discovered architecture-specific interrupt-controller configuration.
///
/// The architecture parser creates this value from firmware data. The value owns the
/// architecture-specific controller configuration. After mapping its MMIO ranges, the BSP
/// publishes the configuration for exarch initialization hooks to consume. Architecture code
/// retains initialized controller state for local-controller setup on the BSP and APs.
pub struct PlatformInterruptController(pub ArchIrqConfig);

/// The pending discovered controller configuration before BSP controller initialization.
static INTERRUPT_CONTROLLER_CONFIG: LazyInit<SpinNoIrq<Option<ArchIrqConfig>>> = LazyInit::new();

impl PlatformInterruptController {
    /// Publishes the discovered configuration for BSP controller initialization.
    ///
    /// Call this after the MMIO regions in the containing platform description are mapped. The
    /// BSP later-initialization hook consumes the configuration once and stores any state required
    /// by local-controller setup on the BSP and APs.
    ///
    /// # Panics
    ///
    /// Panics if another controller configuration was already published.
    pub fn publish(self) {
        INTERRUPT_CONTROLLER_CONFIG.init_once(SpinNoIrq::new(Some(self.0)));
    }
}

/// Takes the discovered controller configuration for one-time global-controller initialization.
///
/// The BSP calls this only after platform discovery and MMIO mapping. The architecture controller
/// then retains the shared state needed by later BSP/AP-local initialization.
///
/// # Panics
///
/// Panics if no configuration was published or the configuration was already consumed.
pub(crate) fn take_interrupt_controller_config() -> ArchIrqConfig {
    INTERRUPT_CONTROLLER_CONFIG
        .get()
        .expect("platform interrupt-controller configuration was not published")
        .lock()
        .take()
        .expect("platform interrupt-controller configuration was already consumed")
}

/// The platform data needed by the common kernel during early device initialization.
pub struct PlatformDescription {
    /// The firmware-discovered CPU topology.
    cpu_topology: CpuTopology,
    /// The platform interrupt-controller driver.
    interrupt_controller: PlatformInterruptController,
    /// The device MMIO ranges that need direct mappings before controller initialization.
    mmio_regions: Vec<PhysMemoryRegion>,
}

impl PlatformDescription {
    /// Creates a platform description from its discovered components.
    pub(crate) fn new(
        cpu_topology: CpuTopology,
        interrupt_controller: PlatformInterruptController,
        mmio_regions: Vec<PhysMemoryRegion>,
    ) -> Self {
        Self {
            cpu_topology,
            interrupt_controller,
            mmio_regions,
        }
    }

    /// Separates the CPU topology, interrupt-controller configuration, and MMIO ranges.
    pub fn into_parts(
        self,
    ) -> (
        CpuTopology,
        PlatformInterruptController,
        Vec<PhysMemoryRegion>,
    ) {
        (
            self.cpu_topology,
            self.interrupt_controller,
            self.mmio_regions,
        )
    }
}

look_at! {
    @crate::arch::current::device:
    /// Discovers the current platform from the boot argument.
    ///
    /// The architecture-specific parser reads ACPI or Device Tree and returns the CPU topology,
    /// controller configuration, and MMIO regions needed by discovered platform devices. This
    /// function must run after the VMM and heap are initialized because firmware parsing may
    /// allocate and access high-half mappings. The caller maps the returned MMIO regions before
    /// invoking the BSP later-initialization hook.
    pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription;
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::CpuTopology;
    use crate::power::PhysicalCpuId;

    /// Preserves the discovered CPU identifiers and bootstrap identifier.
    #[test]
    fn cpu_topology_preserves_discovery_result() {
        let topology = CpuTopology::new(
            vec![
                PhysicalCpuId::new(4),
                PhysicalCpuId::new(7),
                PhysicalCpuId::new(9),
            ]
            .into_boxed_slice(),
            PhysicalCpuId::new(7),
        );

        assert_eq!(
            topology.cpu_ids(),
            &[
                PhysicalCpuId::new(4),
                PhysicalCpuId::new(7),
                PhysicalCpuId::new(9)
            ]
        );
        assert_eq!(topology.bsp_id(), PhysicalCpuId::new(7));
    }

    /// Rejects duplicate physical CPU identifiers.
    #[test]
    #[should_panic(expected = "duplicate CPU identifier")]
    fn cpu_topology_rejects_duplicate_identifiers() {
        let _ = CpuTopology::new(
            vec![
                PhysicalCpuId::new(4),
                PhysicalCpuId::new(7),
                PhysicalCpuId::new(7),
            ]
            .into_boxed_slice(),
            PhysicalCpuId::new(4),
        );
    }
}
