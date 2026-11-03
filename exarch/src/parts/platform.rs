//! Firmware-backed platform discovery.

use alloc::boxed::Box;

use ecraldr_base::PlatformBootArg;
use look_at::look_at;

use crate::{power::PhysicalCpuId, trap::irq::ArchIrqConfig};

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

/// A discovered platform interrupt-controller driver.
///
/// The architecture parser creates this value from firmware data. The value owns the
/// architecture-specific controller configuration, while initialization remains a single common
/// operation for the kernel boot sequence. Interrupt delivery after initialization stays in the
/// architecture-specific fast path.
pub struct PlatformInterruptController(pub ArchIrqConfig);

impl PlatformInterruptController {
    /// Initializes the discovered platform interrupt controller.
    ///
    /// This consumes the driver because the controller configuration is installed into the
    /// architecture's one-time controller state. The caller must invoke this only after the VMM
    /// maps the controller's MMIO regions.
    pub fn initialize(self) {
        crate::trap::irq::init_external_controller(self.0);
        #[cfg(target_arch = "aarch64")]
        crate::arch::aarch64::trap::init_percpu();
    }
}

/// The platform data needed by the common kernel during early device initialization.
pub struct PlatformDescription {
    /// The firmware-discovered CPU topology.
    cpu_topology: CpuTopology,
    /// The platform interrupt-controller driver.
    interrupt_controller: PlatformInterruptController,
}

impl PlatformDescription {
    /// Creates a platform description from its discovered components.
    pub(crate) fn new(
        cpu_topology: CpuTopology,
        interrupt_controller: PlatformInterruptController,
    ) -> Self {
        Self {
            cpu_topology,
            interrupt_controller,
        }
    }

    /// Separates the CPU topology from the platform interrupt-controller driver.
    pub fn into_parts(self) -> (CpuTopology, PlatformInterruptController) {
        (self.cpu_topology, self.interrupt_controller)
    }
}

look_at! {
    @crate::arch::current::device:
    /// Discovers the current platform from the boot argument.
    ///
    /// The architecture-specific parser reads ACPI or Device Tree and returns only the platform
    /// structures required by the common boot sequence. This function must run after the VMM and heap
    /// are initialized because firmware parsing may allocate and access high-half mappings.
    pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription;
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::CpuTopology;

    /// Preserves the discovered CPU identifiers and bootstrap identifier.
    #[test]
    fn cpu_topology_preserves_discovery_result() {
        let topology = CpuTopology::new(vec![4, 7, 9].into_boxed_slice(), 7);

        assert_eq!(topology.cpu_ids(), &[4, 7, 9]);
        assert_eq!(topology.bsp_id(), 7);
    }

    /// Rejects duplicate physical CPU identifiers.
    #[test]
    #[should_panic(expected = "duplicate CPU identifier")]
    fn cpu_topology_rejects_duplicate_identifiers() {
        let _ = CpuTopology::new(vec![4, 7, 7].into_boxed_slice(), 4);
    }
}
