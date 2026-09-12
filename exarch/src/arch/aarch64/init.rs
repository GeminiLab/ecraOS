//! AArch64 implementation of the common platform initialization contract.

use crate::init::PlatformBootArg;
use lazyinit::LazyInit;

use super::trap::Aarch64ExternalIrqConfig;

static GIC_CONFIG: LazyInit<Aarch64ExternalIrqConfig> = LazyInit::new();

/// Discovers the AArch64 controller description from the firmware device tree.
pub fn probe_platform(arg: PlatformBootArg) {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        return;
    };
    let dtb = unsafe {
        fdt_rs::base::DevTree::from_raw_pointer(
            memory_addr::VirtAddr::from_usize(dtb_addr.as_usize()).as_ptr(),
        )
        .expect("failed to parse device tree")
    };
    use fdt_rs::prelude::{FallibleIterator, PropReader};
    for node in dtb.nodes().iterator() {
        let node = node.expect("failed to inspect device tree node");
        if node.name().is_ok_and(|name| name.starts_with("psci"))
            && let Some(method) = node
                .props()
                .find(|prop| Ok(prop.name()? == "method"))
                .expect("failed to inspect PSCI method")
        {
            super::power::set_psci_method(method.str().is_ok_and(|value| value == "hvc"));
        }
        let gic = node
            .props()
            .any(|prop| Ok(prop.name()? == "compatible" && prop.str()? == "arm,gic-v3"))
            .expect("failed to inspect GIC compatibility");
        if !gic {
            continue;
        }
        let Some(reg) = node
            .props()
            .find(|prop| Ok(prop.name()? == "reg"))
            .expect("failed to inspect GIC reg")
        else {
            continue;
        };
        let gicd = reg.u64(0).expect("invalid GIC distributor base");
        let size = reg.u64(1).unwrap_or(0x100000);
        let gicr = reg.u64(2).unwrap_or(gicd + 0x10000);
        GIC_CONFIG.init_once(Aarch64ExternalIrqConfig {
            gicd_base: memory_addr::PhysAddr::from_usize(gicd as usize),
            gicr_base: memory_addr::PhysAddr::from_usize(gicr as usize),
            size: size as usize,
            spi_count: 988,
        });
        break;
    }
}

/// Runs bootstrap-CPU early initialization.
pub fn init_early(arg: PlatformBootArg) {
    super::init_early_bsp(arg);
}

/// Runs late bootstrap-CPU initialization.
pub fn init_later() {
    if let Some(config) = GIC_CONFIG.get() {
        crate::trap::irq::init_external_controller(config.clone());
        super::trap::init_percpu();
    }
}

/// Runs early application-CPU initialization.
pub fn init_early_ap() {
    super::init_early_ap();
}

/// Runs late application-CPU initialization.
pub fn init_later_ap() {
    super::trap::init_percpu();
}
