use alloc::{
    boxed::Box,
    collections::BTreeMap,
    vec::{self, Vec},
};
use core::{
    cmp,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

pub use exarch::power::PhysicalCpuId;
use expercpu::def_percpu;
use expt::pte::MappingFlags;
use lazyinit::LazyInit;
use log::{debug, info, warn};

use crate::{kernel_entry_ap, percpu};

pub type LogicalCpuId = usize;

pub const BSP_CPU_ID: LogicalCpuId = 0;

pub static CPU_LIST: LazyInit<Box<[PhysicalCpuId]>> = LazyInit::new();

static CPU_UP: LazyInit<Box<[AtomicBool]>> = LazyInit::new();

static LOGI_TO_PHYS_ID_MAP: LazyInit<BTreeMap<LogicalCpuId, PhysicalCpuId>> = LazyInit::new();

static PHYS_TO_LOGI_ID_MAP: LazyInit<BTreeMap<PhysicalCpuId, LogicalCpuId>> = LazyInit::new();

#[def_percpu]
pub static CPU_PHYS_ID: usize = 0;

#[def_percpu]
pub static CPU_ID: usize = 0;

pub fn current_cpu_id() -> LogicalCpuId {
    CPU_ID.read_current() as _
}

pub fn current_cpu_phys_id() -> PhysicalCpuId {
    CPU_PHYS_ID.read_current() as _
}

pub fn phys_to_logi_id(phys_id: PhysicalCpuId) -> LogicalCpuId {
    PHYS_TO_LOGI_ID_MAP
        .get()
        .expect("cpu id map must be initialized")
        .get(&phys_id)
        .copied()
        .expect("invalid physical cpu id")
}

/// Initializes the CPU ID for the BSP.
pub fn init_cpu_id_bsp(phys_id: PhysicalCpuId) {
    CPU_PHYS_ID.write_current(phys_id);
    CPU_ID.write_current(BSP_CPU_ID);
}

/// Initializes the CPU ID for the AP.
pub fn init_cpu_id_ap(phys_id: PhysicalCpuId) {
    CPU_PHYS_ID.write_current(phys_id);
    CPU_ID.write_current(phys_to_logi_id(phys_id));
}

pub fn init_cpu_list(cpu_list: Box<[PhysicalCpuId]>, bsp_phys_id: PhysicalCpuId) {
    let mut cpu_list_clone = cpu_list.clone();

    CPU_LIST.init_once(cpu_list);

    if current_cpu_phys_id() != bsp_phys_id {
        warn!(
            "BSP Physical ID mismatch: from loader {:#x}, from ACPI {:#x}",
            current_cpu_phys_id(),
            bsp_phys_id
        );
        warn!("Updating BSP Physical ID to {:#x}", bsp_phys_id);
        CPU_PHYS_ID.write_current(bsp_phys_id);
    }

    info!(
        "{} CPUs detected, BSP Physical ID: {:#x}",
        cpu_list_clone.len(),
        bsp_phys_id
    );

    cpu_list_clone.sort_unstable_by(|a, b| {
        if *a == bsp_phys_id {
            cmp::Ordering::Less
        } else if *b == bsp_phys_id {
            cmp::Ordering::Greater
        } else {
            a.cmp(b)
        }
    });

    let mut logi_to_phys_id_map = BTreeMap::new();
    let mut phys_to_logi_id_map = BTreeMap::new();

    for (logi_id, &phys_id) in cpu_list_clone.iter().enumerate() {
        logi_to_phys_id_map.insert(logi_id, phys_id);
        phys_to_logi_id_map.insert(phys_id, logi_id);
    }

    info!("Logical to Physical ID map: {:x?}", logi_to_phys_id_map);

    LOGI_TO_PHYS_ID_MAP.init_once(logi_to_phys_id_map);
    PHYS_TO_LOGI_ID_MAP.init_once(phys_to_logi_id_map);

    let cpu_up = (0..CPU_LIST.len())
        .map(|i| AtomicBool::new(i == 0))
        .collect::<Vec<_>>();
    CPU_UP.init_once(cpu_up.into_boxed_slice());
}

pub fn start_secondary_cpus() {
    debug_assert_eq!(current_cpu_id(), 0);

    percpu::alloc_percpu_area_for_ap(CPU_LIST.len() - 1);

    let pt_root = crate::mem::vmm::page_table_root();

    for logi_id in 1..CPU_LIST.len() {
        let phys_id = LOGI_TO_PHYS_ID_MAP.deref().get(&logi_id).unwrap();
        info!("Starting up CPU {}: Physical ID {:#x}", logi_id, phys_id);

        let boot_stack = crate::mem::vmalloc::VMALLOC
            .lock()
            .alloc_allocated(
                crate::mem::vmm::page_count_for_bytes(crate::mem::BSP_STACK_SIZE),
                1,
                MappingFlags::WRITE | MappingFlags::READ,
            )
            .unwrap_or_else(|e| panic!("Failed to allocate boot stack for CPU {}: {}", logi_id, e));

        let boot_stack_top = boot_stack.end;

        debug!("Boot stack for CPU {}: {:#x}", logi_id, boot_stack);

        exarch::power::cpu_up(*phys_id, pt_root, boot_stack_top, kernel_entry_ap);

        // Wait for the AP to be up.
        while !CPU_UP.deref().get(logi_id).unwrap().load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

pub fn mark_ap_up(logi_id: LogicalCpuId) {
    CPU_UP
        .deref()
        .get(logi_id)
        .unwrap()
        .store(true, Ordering::Release);
}
