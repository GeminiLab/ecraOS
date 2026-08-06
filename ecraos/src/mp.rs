use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::{
    cmp, fmt,
    ops::Deref,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use exarch::power::CpuStartError;
pub use exarch::power::PhysicalCpuId;
use expercpu::def_percpu;
use expt::pte::MappingFlags;
use lazyinit::LazyInit;
use log::{debug, info, warn};

use crate::{kernel_entry_ap, percpu};

mod state;

pub use state::CpuState;

pub type LogicalCpuId = usize;

pub const BSP_CPU_ID: LogicalCpuId = 0;

pub static CPU_LIST: LazyInit<Box<[PhysicalCpuId]>> = LazyInit::new();

static CPU_STATE: LazyInit<Box<[AtomicU8]>> = LazyInit::new();

static LOGI_TO_PHYS_ID_MAP: LazyInit<BTreeMap<LogicalCpuId, PhysicalCpuId>> = LazyInit::new();

static PHYS_TO_LOGI_ID_MAP: LazyInit<BTreeMap<PhysicalCpuId, LogicalCpuId>> = LazyInit::new();

#[def_percpu]
pub static CPU_PHYS_ID: usize = 0;

#[def_percpu]
pub static CPU_ID: usize = 0;

/// A failure encountered while starting a secondary CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondaryCpuError {
    /// The logical CPU selected for startup.
    pub logical_id: LogicalCpuId,
    /// The firmware physical CPU identifier selected for startup.
    pub physical_id: PhysicalCpuId,
    /// The reason startup stopped progressing.
    pub reason: CpuStartError,
}

impl fmt::Display for SecondaryCpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CPU {} (physical {:#x}) startup failed: {:?}",
            self.logical_id, self.physical_id, self.reason
        )
    }
}

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

    let cpu_state = (0..CPU_LIST.len())
        .map(|i| {
            AtomicU8::new(
                if i == 0 {
                    CpuState::Online
                } else {
                    CpuState::Offline
                }
                .to_u8(),
            )
        })
        .collect::<Vec<_>>();
    CPU_STATE.init_once(cpu_state.into_boxed_slice());
}

pub fn start_secondary_cpus() -> Result<(), SecondaryCpuError> {
    debug_assert_eq!(current_cpu_id(), 0);

    percpu::alloc_percpu_area_for_ap(CPU_LIST.len() - 1);

    let pt_root = crate::mem::vmm::page_table_root();

    for logi_id in 1..CPU_LIST.len() {
        let phys_id = *LOGI_TO_PHYS_ID_MAP.deref().get(&logi_id).unwrap();
        info!("Starting up CPU {}: Physical ID {:#x}", logi_id, phys_id);

        if !transition_cpu_state(logi_id, CpuState::Offline, CpuState::Starting) {
            let state = load_cpu_state(logi_id);
            warn!(
                "CPU {} (physical {:#x}) cannot start from state {:?}",
                logi_id, phys_id, state
            );
            return Err(SecondaryCpuError {
                logical_id: logi_id,
                physical_id: phys_id,
                reason: CpuStartError::InvalidState,
            });
        }

        let boot_stack = crate::mem::allocs::vmalloc::alloc_range_and_map_alloc(
            crate::mem::vmm::page_count_for_bytes(crate::mem::BSP_STACK_SIZE),
            1,
            MappingFlags::WRITE | MappingFlags::READ,
        )
        .map_err(|_| SecondaryCpuError {
            logical_id: logi_id,
            physical_id: phys_id,
            reason: CpuStartError::BootStackError,
        })?;

        let boot_stack_top = boot_stack.end;

        debug!("Boot stack for CPU {}: {:#x}", logi_id, boot_stack);

        if let Err(reason) =
            exarch::power::cpu_up(phys_id, pt_root, boot_stack_top, kernel_entry_ap)
        {
            transition_cpu_state(logi_id, CpuState::Starting, CpuState::Failed);
            return Err(SecondaryCpuError {
                logical_id: logi_id,
                physical_id: phys_id,
                reason,
            });
        }

        // Wait for the AP to be up.
        let deadline = exarch::time::monotonic_time() + Duration::from_secs(1);
        while load_cpu_state(logi_id) != CpuState::Online
            && exarch::time::monotonic_time() < deadline
        {
            core::hint::spin_loop();
        }

        let state = load_cpu_state(logi_id);
        if state != CpuState::Online {
            let observed_at = exarch::time::monotonic_time();
            warn!(
                "CPU {} (physical {:#x}) startup timed out: deadline {:?}, observed at {:?}, state {:?}",
                logi_id, phys_id, deadline, observed_at, state
            );
            transition_cpu_state(logi_id, CpuState::Starting, CpuState::Failed);
            return Err(SecondaryCpuError {
                logical_id: logi_id,
                physical_id: phys_id,
                reason: CpuStartError::Timeout,
            });
        }
    }

    Ok(())
}

pub fn mark_ap_up(logi_id: LogicalCpuId) {
    let _ = transition_cpu_state(logi_id, CpuState::Starting, CpuState::Online);
}

fn load_cpu_state(logi_id: LogicalCpuId) -> CpuState {
    CpuState::from_u8(
        CPU_STATE
            .deref()
            .get(logi_id)
            .expect("cpu state must be initialized")
            .load(Ordering::Acquire),
    )
}

fn transition_cpu_state(logi_id: LogicalCpuId, from: CpuState, to: CpuState) -> bool {
    if !from.can_transition_to(to) {
        return false;
    }

    let state = CPU_STATE
        .deref()
        .get(logi_id)
        .expect("cpu state must be initialized");
    state
        .compare_exchange(
            from.to_u8(),
            to.to_u8(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}
