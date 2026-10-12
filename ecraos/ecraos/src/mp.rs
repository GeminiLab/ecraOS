//! Logical CPU inventory and startup lifecycle.
//!
//! This module owns the immutable logical/physical map and the separate IPI consumer readiness.

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::{
    cmp, fmt,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    time::Duration,
};

use exarch::power::CpuStartError;
pub use exarch::power::PhysicalCpuId;
use expercpu::def_percpu;
use expt::pte::MappingFlags;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::{debug, info, warn};
use memory_addr::VirtAddrRange;

use crate::{kernel_entry_ap, percpu};

/// CPU startup lifecycle states.
///
/// This pure state model rejects invalid boot transitions.
mod state;

pub use state::CpuState;

/// An index in the kernel-owned logical CPU inventory.
///
/// Logical identifiers are stable after firmware discovery.
pub type LogicalCpuId = usize;

/// The logical bootstrap CPU identifier.
///
/// The physical bootstrap identifier is sorted into this slot during discovery.
pub const BSP_CPU_ID: LogicalCpuId = 0;

/// The firmware-discovered physical CPU identifiers.
///
/// Read-only public APIs hide this backing inventory from other kernel consumers.
static CPU_LIST: LazyInit<Box<[PhysicalCpuId]>> = LazyInit::new();

/// The atomic lifecycle state for each logical CPU.
///
/// Online publication follows the completion of AP initialization.
static CPU_STATE: LazyInit<Box<[AtomicU8]>> = LazyInit::new();

/// The separate consumer-readiness state for each logical CPU.
///
/// A Release store publishes mailbox, trap, local-source and scheduler initialization.
static CPU_IPI_READY: LazyInit<Box<[AtomicBool]>> = LazyInit::new();

/// The immutable logical-to-physical CPU translation.
///
/// Architecture transports receive only the physical result of this lookup.
static LOGI_TO_PHYS_ID_MAP: LazyInit<BTreeMap<LogicalCpuId, PhysicalCpuId>> = LazyInit::new();

/// The immutable physical-to-logical CPU translation.
///
/// AP entry uses it to select the kernel-owned per-CPU slot.
static PHYS_TO_LOGI_ID_MAP: LazyInit<BTreeMap<PhysicalCpuId, LogicalCpuId>> = LazyInit::new();

/// Boot-stack usable ranges awaiting adoption by their AP root tasks.
///
/// The lock is never held across CPU startup or acknowledgement waits.
static AP_BOOT_STACK_RANGES: SpinNoIrq<BTreeMap<LogicalCpuId, VirtAddrRange>> =
    SpinNoIrq::new(BTreeMap::new());

/// The current CPU's firmware physical identifier.
///
/// Per-CPU storage is initialized before controller and scheduler setup.
#[def_percpu]
pub static CPU_PHYS_ID: usize = 0;

/// The current CPU's logical inventory index.
///
/// The immutable translation map determines this per-CPU value.
#[def_percpu]
pub static CPU_ID: usize = 0;

/// A failure encountered while starting a secondary CPU.
///
/// Logical and physical targets remain available when firmware or initialization fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondaryCpuError {
    /// The logical CPU selected for startup.
    ///
    /// It indexes the immutable common inventory.
    pub logical_id: LogicalCpuId,
    /// The firmware physical CPU identifier selected for startup.
    ///
    /// Architecture startup receives this destination.
    pub physical_id: PhysicalCpuId,
    /// The reason startup stopped progressing.
    ///
    /// Errors preserve firmware and lifecycle failure classification.
    pub reason: CpuStartError,
}

impl fmt::Display for SecondaryCpuError {
    /// Formats the CPU destination and startup failure.
    ///
    /// Human-readable diagnostics retain both identifier namespaces.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CPU {} (physical {:#x}) startup failed: {:?}",
            self.logical_id, self.physical_id, self.reason
        )
    }
}

/// Returns the executing CPU's logical identifier.
///
/// Callers prevent migration when using the value across a critical operation.
pub fn current_cpu_id() -> LogicalCpuId {
    CPU_ID.read_current() as _
}

/// Returns the executing CPU's firmware physical identifier.
///
/// Architecture mechanisms use this namespace for controller targeting.
pub fn current_cpu_phys_id() -> PhysicalCpuId {
    CPU_PHYS_ID.read_current() as _
}

/// Returns the number of CPUs discovered by the platform.
///
/// The value is stable after the platform initializes the CPU list.
pub fn cpu_count() -> usize {
    CPU_LIST.len()
}

/// Resolves a logical CPU to its firmware physical identifier.
///
/// Invalid identifiers and calls before discovery return `None`.
pub fn logical_to_physical(logical_id: LogicalCpuId) -> Option<PhysicalCpuId> {
    logical_to_physical_from_map(LOGI_TO_PHYS_ID_MAP.get()?, logical_id)
}

/// Resolves one logical CPU against an immutable translation map.
///
/// The shared helper keeps invalid-target handling testable without boot initialization.
fn logical_to_physical_from_map(
    map: &BTreeMap<LogicalCpuId, PhysicalCpuId>,
    logical_id: LogicalCpuId,
) -> Option<PhysicalCpuId> {
    map.get(&logical_id).copied()
}

/// Returns the lifecycle state for a valid discovered CPU.
///
/// Acquire ordering observes initialization preceding an Online transition.
pub fn cpu_state(logical_id: LogicalCpuId) -> Option<CpuState> {
    Some(CpuState::from_u8(
        CPU_STATE.get()?.get(logical_id)?.load(Ordering::Acquire),
    ))
}

/// Returns whether a CPU has initialized all local IPI consumers.
///
/// This is separate from lifecycle state and never substitutes for an Online check.
pub fn is_ipi_ready(logical_id: LogicalCpuId) -> bool {
    CPU_IPI_READY
        .get()
        .and_then(|ready| ready.get(logical_id))
        .is_some_and(|ready| ready.load(Ordering::Acquire))
}

/// Selects the intersection of online and IPI-ready CPU states.
///
/// No CPU can leave Online in this implementation, so the resulting owned snapshot is stable.
fn ready_snapshot(states: &[CpuState], readiness: &[bool]) -> Vec<LogicalCpuId> {
    states
        .iter()
        .zip(readiness)
        .enumerate()
        .filter_map(|(id, (&state, &ready))| (state == CpuState::Online && ready).then_some(id))
        .collect()
}

/// Captures an owned snapshot of online CPUs with usable IPI consumers.
///
/// Callers allocate this snapshot before taking mapping or scheduler critical-section locks.
pub fn ipi_ready_cpu_ids() -> Vec<LogicalCpuId> {
    let Some(states) = CPU_STATE.get() else {
        return Vec::new();
    };
    let states = states
        .iter()
        .map(|state| CpuState::from_u8(state.load(Ordering::Acquire)))
        .collect::<Vec<_>>();
    let readiness = (0..states.len()).map(is_ipi_ready).collect::<Vec<_>>();
    ready_snapshot(&states, &readiness)
}

/// Publishes the current CPU's completed IPI initialization.
///
/// Boot calls this only after common mailboxes, traps, the local source and scheduler are usable.
pub(crate) fn mark_current_cpu_ipi_ready() {
    CPU_IPI_READY[current_cpu_id()].store(true, Ordering::Release);
}

/// Returns the boot stack range for the current CPU.
///
/// Scheduler initialization adopts this complete usable range.
pub fn current_cpu_boot_stack_range() -> VirtAddrRange {
    cpu_boot_stack_range(current_cpu_id()).expect("Failed to get current CPU boot stack range")
}

/// Returns the boot stack range for a specific CPU.
///
/// AP ranges remain owned by startup metadata until adopted by the scheduler.
pub fn cpu_boot_stack_range(logi_id: LogicalCpuId) -> Option<VirtAddrRange> {
    if logi_id == BSP_CPU_ID {
        return Some(crate::mem::bsp_stack_range());
    }

    AP_BOOT_STACK_RANGES.lock().get(&logi_id).copied()
}

/// Resolves the physical identifier of a discovered CPU.
///
/// AP entry requires a known destination and treats unknown firmware IDs as an invariant failure.
pub fn phys_to_logi_id(phys_id: PhysicalCpuId) -> LogicalCpuId {
    PHYS_TO_LOGI_ID_MAP
        .get()
        .expect("cpu id map must be initialized")
        .get(&phys_id)
        .copied()
        .expect("invalid physical cpu id")
}

/// Initializes the CPU ID for the BSP.
///
/// Discovery may subsequently correct the firmware physical identifier.
pub fn init_cpu_id_bsp(phys_id: PhysicalCpuId) {
    CPU_PHYS_ID.write_current(phys_id);
    CPU_ID.write_current(BSP_CPU_ID);
}

/// Initializes the CPU ID for the AP.
///
/// The logical slot is resolved through the common immutable map.
pub fn init_cpu_id_ap(phys_id: PhysicalCpuId) {
    CPU_PHYS_ID.write_current(phys_id);
    CPU_ID.write_current(phys_to_logi_id(phys_id));
}

/// Installs the immutable CPU inventory and initial lifecycle state.
///
/// The BSP occupies logical slot zero and all readiness bits begin clear.
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
    CPU_IPI_READY.init_once(
        (0..CPU_LIST.len())
            .map(|_| AtomicBool::new(false))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
}

/// Starts discovered APs and waits for their bounded Online publication.
///
/// Boot stacks and per-CPU areas are allocated before firmware startup.
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

        AP_BOOT_STACK_RANGES.lock().insert(logi_id, boot_stack);

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

/// Publishes a fully initialized application CPU as Online.
///
/// Common boot publishes IPI readiness immediately before this transition.
pub fn mark_ap_up(logi_id: LogicalCpuId) {
    let _ = transition_cpu_state(logi_id, CpuState::Starting, CpuState::Online);
}

/// Observes one initialized CPU lifecycle state.
///
/// Acquire ordering pairs with the AP initialization publication.
fn load_cpu_state(logi_id: LogicalCpuId) -> CpuState {
    CpuState::from_u8(
        CPU_STATE
            .deref()
            .get(logi_id)
            .expect("cpu state must be initialized")
            .load(Ordering::Acquire),
    )
}

/// Applies one valid atomic lifecycle transition.
///
/// Rejected transitions preserve the old state and never fabricate successful startup.
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

/// Ready-target snapshot tests.
///
/// These checks reject CPUs whose lifecycle or mailbox readiness cannot accept an IPI.
#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec};

    use super::{CpuState, logical_to_physical_from_map, ready_snapshot};

    /// Rejects online CPUs that have not initialized their IPI consumer.
    ///
    /// Online publication alone must never qualify a target for delivery.
    #[test]
    fn snapshot_requires_separate_readiness() {
        assert_eq!(
            ready_snapshot(&[CpuState::Online, CpuState::Online], &[true, false]),
            vec![0]
        );
    }

    /// Rejects failed and starting CPUs even when their readiness bit is set.
    ///
    /// A stable snapshot contains only the intersection of online and ready CPUs.
    #[test]
    fn snapshot_requires_online_lifecycle() {
        assert_eq!(
            ready_snapshot(
                &[
                    CpuState::Online,
                    CpuState::Failed,
                    CpuState::Starting,
                    CpuState::Offline
                ],
                &[true, true, true, true]
            ),
            vec![0]
        );
    }

    /// Returns no physical destination for an invalid logical identifier.
    ///
    /// Invalid targets must fail before slot reservation or hardware submission.
    #[test]
    fn logical_lookup_is_fallible() {
        let map = BTreeMap::from([(0, 7), (1, 42)]);
        assert_eq!(logical_to_physical_from_map(&map, 1), Some(42));
        assert_eq!(logical_to_physical_from_map(&map, 99), None);
    }
}
