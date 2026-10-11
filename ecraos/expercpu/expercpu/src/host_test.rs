//! Host-side per-CPU area management.
//!
//! Provides process-backed per-CPU storage for logic tests that cannot use a
//! kernel CPU register as the current per-CPU base.

use std::{
    alloc,
    cell::Cell,
    num::NonZeroU32,
    ptr::NonNull,
    sync::{Mutex, OnceLock},
};

use memory_addr::VirtAddr;

/// The CPU count used when a test does not initialize the host backend.
///
/// A single logical CPU is sufficient for ordinary unit tests. Tests that
/// model more CPUs can call [`initialize`] before binding their threads.
const DEFAULT_CPU_COUNT: usize = 1;

/// The process-wide logical CPU count.
///
/// The first explicit initialization fixes the count for the remainder of the
/// test process so concurrent tests observe a consistent configuration.
static CPU_COUNT: OnceLock<usize> = OnceLock::new();

/// Allocated per-CPU areas retained for the lifetime of the test process.
///
/// Per-CPU variables can contain values without a generally valid destructor,
/// so host areas intentionally follow the kernel's process-lifetime semantics.
static AREAS: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();

thread_local! {
    /// The current thread's logical CPU identifier and per-CPU base.
    ///
    /// Each host test thread receives an independent area on first access.
    static CURRENT_AREA: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

/// Initializes process-lifetime per-CPU storage for host tests.
///
/// Repeated initialization is accepted when the requested CPU count matches
/// the existing configuration and rejected otherwise.
pub fn initialize(cpu_count: NonZeroU32) {
    let requested = cpu_count.get() as usize;
    match CPU_COUNT.set(requested) {
        Ok(()) => {}
        Err(_) => assert_eq!(
            self::cpu_count(),
            requested,
            "host per-CPU count already differs"
        ),
    }
    AREAS.get_or_init(|| Mutex::new(Vec::new()));
}

/// Binds the current host thread to a per-CPU area.
///
/// The first call allocates and initializes an area from the linker-provided
/// template. A thread cannot be rebound to a different logical CPU.
pub fn bind_current_cpu(cpu_id: usize) -> VirtAddr {
    assert!(cpu_id < cpu_count(), "invalid host CPU id {cpu_id}");
    CURRENT_AREA.with(|current| {
        if let Some((current_cpu_id, base)) = current.get() {
            assert_eq!(
                current_cpu_id, cpu_id,
                "host thread is already bound to CPU {current_cpu_id}"
            );
            return VirtAddr::from_usize(base);
        }

        let base = allocate_area();
        current.set(Some((cpu_id, base)));
        unsafe {
            crate::init(VirtAddr::from_usize(base));
        }
        VirtAddr::from_usize(base)
    })
}

/// Returns the current host thread's initialized per-CPU area.
///
/// Unbound threads are lazily bound to logical CPU zero so ordinary logic
/// tests do not need a test-harness initialization hook.
pub fn current_base() -> VirtAddr {
    CURRENT_AREA.with(|current| {
        current
            .get()
            .map(|(_, base)| VirtAddr::from_usize(base))
            .unwrap_or_else(|| bind_current_cpu(0))
    })
}

/// Sets the current host thread's per-CPU base.
///
/// Preserves an existing logical CPU binding. Direct calls through
/// [`crate::init`] bind an otherwise unbound thread to logical CPU zero.
pub(crate) fn set_current_base(base: VirtAddr) {
    CURRENT_AREA.with(|current| {
        let cpu_id = current.get().map(|(cpu_id, _)| cpu_id).unwrap_or(0);
        current.set(Some((cpu_id, base.as_usize())));
    });
}

/// Returns the number of host CPUs available to the test process.
///
/// Returns one until [`initialize`] supplies an explicit process-wide count.
pub fn cpu_count() -> usize {
    CPU_COUNT.get().copied().unwrap_or(DEFAULT_CPU_COUNT)
}

/// Allocates and retains one host per-CPU area.
///
/// Returns the raw address needed at the low-level storage boundary. The area
/// is zeroed before [`crate::init`] copies the per-CPU template into it.
fn allocate_area() -> usize {
    let layout = crate::percpu_area_layout();
    let pointer = unsafe { alloc::alloc_zeroed(layout) };
    let pointer = NonNull::new(pointer).expect("host per-CPU area allocation failed");
    let address = pointer.as_ptr() as usize;

    let areas = AREAS.get_or_init(|| Mutex::new(Vec::new()));
    areas
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(address);
    address
}
