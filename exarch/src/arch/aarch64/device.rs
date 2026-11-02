//! AArch64 Device Tree platform discovery.

use alloc::vec::Vec;

use ecraldr_base::{PhysAddr, PlatformBootArg};
use fdt_rs::{base::DevTree, prelude::*};

use crate::{
    platform::{
        CpuTopology, PlatformDescription, PlatformInterruptController, dt::DeviceTreeNodeExt,
    },
    trap::irq::Aarch64ExternalIrqConfig,
};

/// Discovers the AArch64 CPU topology, PSCI conduit, and GICv3 configuration.
pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        panic!("AArch64 platform discovery requires a Device Tree")
    };
    let dtb = unsafe {
        DevTree::from_raw_pointer(crate::kernel_if::phys_to_virt(dtb_addr).as_ptr())
            .expect("failed to parse device tree")
    };
    let mut cpu_address_cells = None;
    for node in dtb.nodes().iterator() {
        let node = node.expect("failed to inspect device-tree node");
        if node
            .name()
            .is_ok_and(|name| name.split('@').next() == Some("cpus"))
        {
            cpu_address_cells = node
                .props()
                .find(|prop| Ok(prop.name()? == "#address-cells"))
                .expect("failed to inspect CPU address-cell property")
                .and_then(|prop| prop.u32(0).ok())
                .map(|cells| cells as usize);
            break;
        }
    }
    let cpu_address_cells = cpu_address_cells.unwrap_or(2);
    assert!(
        matches!(cpu_address_cells, 1 | 2),
        "unsupported CPU address-cell count: {cpu_address_cells}"
    );
    let mut cpu_ids = Vec::new();
    for node in dtb.nodes().iterator() {
        let node = node.expect("failed to inspect device-tree node");
        if node.name().is_ok_and(|name| name.starts_with("psci")) {
            if let Some(method) = node
                .props()
                .find(|prop| Ok(prop.name()? == "method"))
                .expect("failed to inspect PSCI method")
            {
                super::power::set_psci_method(method.str().is_ok_and(|value| value == "hvc"));
            }
        }
        let is_cpu = node
            .is_device_type("cpu")
            .expect("failed to inspect CPU type");
        if is_cpu {
            let enabled = node.is_enabled().expect("failed to inspect CPU status");
            if enabled {
                let reg = node
                    .property("reg")
                    .expect("failed to inspect CPU reg")
                    .expect("CPU node has no reg property");
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
    }
    let bsp_id = crate::kernel_if::current_cpu_phys_id();
    let cpu_topology = CpuTopology::new(cpu_ids.into_boxed_slice(), bsp_id);

    let gic_node = dtb
        .compatible_nodes("arm,gic-v3")
        .next()
        .expect("failed to inspect GIC compatible")
        .expect("Device Tree has no GICv3 controller");
    let reg = gic_node
        .property("reg")
        .expect("failed to inspect GIC reg")
        .expect("GIC node has no reg property");
    let gic = Aarch64ExternalIrqConfig {
        gicd_base: PhysAddr::from_usize(reg.u64(0).expect("invalid GIC distributor base") as usize),
        gicd_size: reg.u64(1).expect("invalid GIC distributor size") as usize,
        gicr_base: PhysAddr::from_usize(
            reg.u64(2).expect("invalid GIC redistributor base") as usize
        ),
        gicr_size: reg.u64(3).expect("invalid GIC redistributor size") as usize,
        spi_count: 988,
    };
    PlatformDescription::new(cpu_topology, PlatformInterruptController::Aarch64(gic))
}
