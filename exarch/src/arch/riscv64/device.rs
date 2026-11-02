//! RISC-V Device Tree platform discovery.

use alloc::{collections::BTreeMap, vec::Vec};

use ecraldr_base::{PhysAddr, PlatformBootArg};
use fdt_rs::{base::DevTree, index::DevTreeIndex, prelude::*};

use crate::{
    platform::{
        CpuTopology, PlatformDescription, PlatformInterruptController, dt::DeviceTreeNodeExt,
    },
    power::PhysicalCpuId,
    trap::irq::{RiscvExternalIrqConfig, RiscvPlicContext},
};

/// Discovers the RISC-V CPU topology and PLIC from Device Tree.
pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        panic!("RISC-V platform discovery requires a Device Tree")
    };
    let dtb = unsafe {
        DevTree::from_raw_pointer(crate::kernel_if::phys_to_virt(dtb_addr).as_ptr())
            .expect("failed to load device tree")
    };
    let layout = DevTreeIndex::get_layout(&dtb).expect("failed to get device-tree index layout");
    let buffer_ptr = unsafe { alloc::alloc::alloc(layout) };
    if buffer_ptr.is_null() {
        alloc::alloc::handle_alloc_error(layout);
    }

    let (cpu_topology, controller) = {
        let buffer = unsafe { core::slice::from_raw_parts_mut(buffer_ptr, layout.size()) };
        let index = DevTreeIndex::new(dtb, buffer).expect("failed to create device-tree index");

        let dtb_boot_cpu = index.fdt().boot_cpuid_phys() as PhysicalCpuId;
        let boot_cpu = crate::kernel_if::current_cpu_phys_id();
        if dtb_boot_cpu != boot_cpu {
            log::warn!(
                "Device Tree boot CPU ID {dtb_boot_cpu:#x} differs from runtime hart ID {boot_cpu:#x}; using runtime hart ID as BSP"
            );
        }
        let mut cpu_ids = Vec::new();
        for node in index.fdt().nodes().iterator() {
            let node = node.expect("failed to inspect CPU node");
            let is_cpu = node
                .is_device_type("cpu")
                .expect("failed to inspect CPU type");
            if !is_cpu {
                continue;
            }
            let enabled = node.is_enabled().expect("failed to inspect CPU status");
            if !enabled {
                continue;
            }
            let reg = node
                .props()
                .find(|prop| Ok(prop.name()? == "reg"))
                .expect("failed to inspect CPU reg")
                .expect("CPU node has no reg property");
            let cpu_id = reg.u32(0).expect("invalid CPU identifier") as PhysicalCpuId;
            cpu_ids.push(cpu_id);
        }
        let cpu_topology = CpuTopology::new(cpu_ids.into_boxed_slice(), boot_cpu);

        let plic_node = index
            .compatible_nodes("sifive,plic-1.0.0")
            .next()
            .or_else(|| index.compatible_nodes("riscv,plic0").next())
            .expect("Device Tree has no supported PLIC node");
        let plic_reg = plic_node
            .props()
            .find(|prop| prop.name().is_ok_and(|name| name == "reg"))
            .expect("PLIC node has no reg property");
        let plic_ndev = plic_node
            .props()
            .find(|prop| prop.name().is_ok_and(|name| name == "riscv,ndev"))
            .expect("PLIC node has no riscv,ndev property")
            .u32(0)
            .expect("invalid PLIC source count") as usize;
        let interrupts = plic_node
            .props()
            .find(|prop| prop.name().is_ok_and(|name| name == "interrupts-extended"))
            .expect("PLIC node has no interrupts-extended property");

        let mut interrupt_controllers = BTreeMap::new();
        for node in index.nodes() {
            let Some(phandle) = node
                .props()
                .find(|prop| prop.name().is_ok_and(|name| name == "phandle"))
                .and_then(|prop| prop.phandle(0).ok())
            else {
                continue;
            };
            let is_controller = node
                .props()
                .any(|prop| prop.name().is_ok_and(|name| name == "interrupt-controller"));
            if !is_controller {
                continue;
            }
            let Some(cpu_node) = node.parent() else {
                continue;
            };
            let Some(hart_id) = cpu_node
                .props()
                .find(|prop| prop.name().is_ok_and(|name| name == "reg"))
                .and_then(|prop| prop.u32(0).ok())
            else {
                continue;
            };
            interrupt_controllers.insert(phandle, hart_id as PhysicalCpuId);
        }

        let mut contexts = Vec::new();
        for entry in 0..(interrupts.length() / 8) {
            let phandle = interrupts
                .phandle(entry * 2)
                .expect("invalid PLIC context phandle");
            let cause = interrupts
                .u32(entry * 2 + 1)
                .expect("invalid PLIC context cause");
            if cause == 9
                && let Some(&hart_id) = interrupt_controllers.get(&phandle)
            {
                contexts.push(RiscvPlicContext {
                    hart_id,
                    context: entry,
                });
            }
        }
        assert!(!contexts.is_empty(), "PLIC has no supervisor contexts");

        let controller = RiscvExternalIrqConfig {
            physical_base: PhysAddr::from_usize(
                plic_reg.u64(0).expect("invalid PLIC base") as usize
            ),
            size: plic_reg.u64(1).expect("invalid PLIC size") as usize,
            source_count: plic_ndev,
            contexts: contexts.into_boxed_slice(),
        };
        (cpu_topology, controller)
    };
    unsafe { alloc::alloc::dealloc(buffer_ptr, layout) };
    PlatformDescription::new(
        cpu_topology,
        PlatformInterruptController::RiscV64(controller),
    )
}
