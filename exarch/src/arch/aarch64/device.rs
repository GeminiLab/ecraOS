//! AArch64 Device Tree platform discovery.

use alloc::vec::Vec;

use ecraldr_base::{PhysAddr, PlatformBootArg};
use fdt_rs::{base::DevTree, prelude::PropReader};

use crate::{
    platform::{
        CpuTopology, PlatformDescription, PlatformInterruptController,
        dt::{
            DeviceTreeNodeExt,
            index::{DeviceTreeIndexExt, with_index},
        },
    },
    trap::irq::Aarch64ExternalIrqConfig,
};

/// Discovers the AArch64 CPU topology, PSCI conduit, and GICv3 configuration.
pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        panic!("AArch64 platform discovery requires a Device Tree")
    };
    // SAFETY: the loader supplies a valid DTB that remains mapped during platform discovery.
    let dtb = unsafe {
        DevTree::from_raw_pointer(crate::kernel_if::phys_to_virt(dtb_addr).as_ptr())
            .expect("failed to parse device tree")
    };
    with_index(dtb, |index| {
        let cpus = index
            .find_path("/cpus")?
            .expect("Device Tree has no /cpus node");
        let cpu_address_cells = cpus.u32_property("#address-cells", 0)?.unwrap_or(2);
        assert!(
            matches!(cpu_address_cells, 1 | 2),
            "unsupported CPU address-cell count: {cpu_address_cells}"
        );
        let mut cpu_ids = Vec::new();
        for node in index.nodes() {
            if node.is_node_kind("psci")?
                && let Some(method) = node.string_property("method")?
            {
                super::power::set_psci_method(method == "hvc");
            }
        }
        for node in cpus.children() {
            if node.is_device_type("cpu")? && node.is_enabled()? {
                let reg = node.property("reg")?.expect("CPU node has no reg property");
                let affinity = match cpu_address_cells {
                    1 => u64::from(reg.u32(0).expect("invalid CPU affinity")),
                    2 => {
                        let high = u64::from(reg.u32(0).expect("invalid CPU affinity"));
                        let low = u64::from(reg.u32(1).expect("invalid CPU affinity"));
                        (high << 32) | low
                    }
                    _ => unreachable!("CPU address-cell count validated above"),
                };
                cpu_ids.push(usize::try_from(affinity).expect("CPU affinity exceeds usize"));
            }
        }
        let bsp_id = crate::kernel_if::current_cpu_phys_id();
        let cpu_topology = CpuTopology::new(cpu_ids.into_boxed_slice(), bsp_id);

        let gic_node = index
            .find_compatible("arm,gic-v3")?
            .expect("Device Tree has no GICv3 controller");
        let reg = gic_node
            .property("reg")?
            .expect("GIC node has no reg property");
        let gic = Aarch64ExternalIrqConfig {
            gicd_base: PhysAddr::from_usize(
                reg.u64(0).expect("invalid GIC distributor base") as usize
            ),
            gicd_size: reg.u64(1).expect("invalid GIC distributor size") as usize,
            gicr_base: PhysAddr::from_usize(
                reg.u64(2).expect("invalid GIC redistributor base") as usize
            ),
            gicr_size: reg.u64(3).expect("invalid GIC redistributor size") as usize,
            spi_count: 988,
        };
        Ok(PlatformDescription::new(
            cpu_topology,
            PlatformInterruptController(gic),
        ))
    })
    .expect("failed to discover AArch64 platform from Device Tree")
}
