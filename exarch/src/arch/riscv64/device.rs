//! RISC-V Device Tree platform discovery.

use alloc::{collections::BTreeMap, vec::Vec};

use ecraldr_base::{PhysAddr, PlatformBootArg};
use fdt_rs::{base::DevTree, prelude::PropReader};

use crate::{
    mem::{DEFAULT_MMIO_FLAGS, MemoryRegion},
    platform::{
        CpuTopology, PlatformDescription, PlatformInterruptController,
        dt::{
            DeviceTreeNodeExt,
            index::{DeviceTreeIndexExt, with_index},
        },
    },
    power::PhysicalCpuId,
    trap::irq::{RiscvExternalIrqConfig, RiscvPlicContext},
};

/// Discovers the RISC-V CPU topology and PLIC from Device Tree.
pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        panic!("RISC-V platform discovery requires a Device Tree")
    };
    // SAFETY: the loader supplies a valid DTB that remains mapped during platform discovery.
    let dtb = unsafe {
        DevTree::from_raw_pointer(crate::kernel_if::phys_to_virt(dtb_addr).as_ptr())
            .expect("failed to load device tree")
    };
    with_index(dtb, |index| {
        let dtb_boot_cpu = index.fdt().boot_cpuid_phys() as PhysicalCpuId;
        let boot_cpu = crate::kernel_if::current_cpu_phys_id();
        if dtb_boot_cpu != boot_cpu {
            log::warn!(
                "Device Tree boot CPU ID {dtb_boot_cpu:#x} differs from runtime hart ID {boot_cpu:#x}; using runtime hart ID as BSP"
            );
        }
        let cpus = index
            .find_path("/cpus")?
            .expect("Device Tree has no /cpus node");
        let mut cpu_ids = Vec::new();
        for node in cpus.children() {
            if !node.is_device_type("cpu")? || !node.is_enabled()? {
                continue;
            }
            let cpu_id = node
                .u32_property("reg", 0)?
                .expect("CPU node has no reg property") as PhysicalCpuId;
            cpu_ids.push(cpu_id);
        }
        let cpu_topology = CpuTopology::new(cpu_ids.into_boxed_slice(), boot_cpu);

        let plic_node = match index.find_compatible("sifive,plic-1.0.0")? {
            Some(node) => node,
            None => index
                .find_compatible("riscv,plic0")?
                .expect("Device Tree has no supported PLIC node"),
        };
        assert!(plic_node.is_enabled()?, "Device Tree PLIC is disabled");
        let plic_reg = plic_node
            .property("reg")?
            .expect("PLIC node has no reg property");
        let plic_ndev = plic_node
            .u32_property("riscv,ndev", 0)?
            .expect("PLIC node has no riscv,ndev property") as usize;
        let interrupts = plic_node
            .property("interrupts-extended")?
            .expect("PLIC node has no interrupts-extended property");

        let mut interrupt_controllers = BTreeMap::new();
        for node in index.nodes() {
            if !node.has_property("interrupt-controller")? {
                continue;
            }
            let Some(phandle) = node.phandle_property("phandle")? else {
                continue;
            };
            let Some(cpu_node) = node.parent() else {
                continue;
            };
            if !cpu_node.is_device_type("cpu")? {
                continue;
            }
            let Some(hart_id) = cpu_node.u32_property("reg", 0)? else {
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
        let mmio_regions = alloc::vec![MemoryRegion {
            range: memory_addr::PhysAddrRange::from_start_size(
                controller.physical_base,
                controller.size,
            ),
            flags: DEFAULT_MMIO_FLAGS,
            desc: "PLIC registers",
        }];
        Ok(PlatformDescription::new(
            cpu_topology,
            PlatformInterruptController(controller),
            mmio_regions,
        ))
    })
    .expect("failed to discover RISC-V platform from Device Tree")
}
