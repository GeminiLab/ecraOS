//! Preemption state and deferred scheduling requests.
//!
//! This module keeps the scheduler's interrupt-safe preemption contract independent from the
//! architecture trap code. Remote notification sets the same pending state as a local timer.

use core::sync::atomic::{AtomicBool, Ordering};

use expercpu::def_percpu;

/// Per-CPU preemption nesting depth.
///
/// Kernel guards balance this counter around task migration boundaries.
#[def_percpu]
static PREEMPT_DEPTH: usize = 0;

/// Per-CPU deferred scheduling request.
///
/// Local reason dispatch publishes it and policy-aware safe points consume it.
#[def_percpu]
static RESCHEDULE_PENDING: AtomicBool = AtomicBool::new(false);

/// A guard that defers preemption until dropped.
///
/// Interrupt delivery remains enabled so common IPI handlers can progress.
pub struct RescheduleGuard {
    /// The balanced kernel preemption guard.
    ///
    /// Destruction restores the caller's prior nesting depth.
    inner: kernel_guard::NoPreempt,
}

impl RescheduleGuard {
    /// Disables preemption on the current CPU.
    ///
    /// Local interrupts retain their current state.
    pub fn new() -> Self {
        Self {
            // Destruction restores the caller's prior nesting depth.
            inner: kernel_guard::NoPreempt::new(),
        }
    }
}

impl Default for RescheduleGuard {
    /// Creates a preemption guard with its standard constructor.
    ///
    /// Default construction follows the same balanced nesting contract.
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RescheduleGuard {
    /// Re-enables preemption and preserves any deferred request.
    ///
    /// Switching occurs only at an explicit safe point.
    fn drop(&mut self) {
        let _ = &self.inner;
    }
}

/// Disables preemption for the current CPU through the `kernel_guard` bridge.
///
/// Raw local access prevents recursively constructing another preemption guard.
pub fn disable_preempt() {
    // SAFETY: KernelGuardIf invokes this before a guard can permit scheduler migration. Raw
    // per-CPU access avoids recursively acquiring NoPreempt through generated helpers.
    unsafe {
        let depth = PREEMPT_DEPTH.current_ref_mut_raw();
        *depth = depth.checked_add(1).expect("preemption depth overflow");
    }
}

/// Enables preemption for the current CPU through the `kernel_guard` bridge.
///
/// Underflow denotes an unmatched guard and is a fatal invariant failure.
pub fn enable_preempt() {
    // SAFETY: KernelGuardIf invokes this while its matching NoPreempt guard is being released.
    // Raw access avoids recursively reacquiring the same guard.
    unsafe {
        let depth = PREEMPT_DEPTH.current_ref_mut_raw();
        *depth = depth.checked_sub(1).expect("preemption depth underflow");
    }
}

/// Returns whether a scheduler context switch is currently permitted.
///
/// Task-context blocking requires both zero nesting and enabled local interrupts.
pub fn can_schedule_now() -> bool {
    // SAFETY: This is a diagnostic safe-point check and callers already serialize task state.
    (unsafe { *PREEMPT_DEPTH.current_ref_raw() == 0 }) && exarch::trap::local_enabled()
}

/// Returns whether an interrupt-exit scheduling point may preempt the local task.
///
/// Local interrupts are necessarily disabled while an interrupt handler executes, so this predicate
/// intentionally tests only the `kernel_guard`-backed preemption depth. Ordinary blocking APIs
/// continue to use their stricter task-context checks.
pub fn can_preempt_from_interrupt() -> bool {
    // SAFETY: The interrupt-exit hook reads only its local per-CPU nesting counter before deciding
    // whether to consume a deferred request.
    unsafe { *PREEMPT_DEPTH.current_ref_raw() == 0 }
}

/// Requests a local reschedule at the next safe point.
///
/// The software reason handler publishes intent without acquiring scheduler queue locks.
pub fn request_reschedule() {
    // SAFETY: Queue publication precedes this Release store. Only the owning CPU writes this per-CPU bit.
    // Remote producers publish the common Reschedule reason instead.
    unsafe {
        RESCHEDULE_PENDING
            .current_ref_raw()
            .store(true, Ordering::Release)
    }
}

/// Requests scheduling on the queue's selected logical CPU.
///
/// Producers call this after queue publication and after releasing all queue and domain locks.
pub(crate) fn request_reschedule_on(
    target: crate::mp::LogicalCpuId,
) -> Result<(), crate::ipi::IpiError> {
    let _guard = kernel_guard::NoPreempt::new();
    if target == crate::mp::current_cpu_id() {
        request_reschedule();
        Ok(())
    } else {
        crate::ipi::notify(target, crate::ipi::IpiReason::Reschedule)
    }
}

/// Accounts one timer tick for the current task and defers preemption when its slice expires.
///
/// A cooperative task has no timer slice and cannot become interrupt-preemptible.
pub fn timer_tick() {
    let Some(task) = super::scheduler::current_task_ref() else {
        return;
    };
    if task.account_tick() {
        request_reschedule();
    }
}

/// Consumes one pending request with acquire-release ordering.
///
/// Only policy-aware scheduling safe points may clear the local request.
pub fn take_reschedule_request() -> bool {
    take_reschedule_request_if(true)
}

/// Consumes a pending request only when the safe-point policy permits switching.
///
/// A deferred cooperative or guarded interrupt exit leaves the atomic indication untouched.
pub(super) fn take_reschedule_request_if(permitted: bool) -> bool {
    if !permitted {
        return false;
    }
    // SAFETY: The local safe point consumes this CPU's request with AcqRel ordering, so a new
    // publisher after the swap leaves the bit set for the following safe point.
    unsafe {
        RESCHEDULE_PENDING
            .current_ref_raw()
            .swap(false, Ordering::AcqRel)
    }
}

/// Returns the current preemption nesting depth for diagnostics and tests.
///
/// The value describes only the executing CPU.
pub fn depth() -> usize {
    // SAFETY: Callers use this only as a local diagnostic snapshot.
    unsafe { *PREEMPT_DEPTH.current_ref_raw() }
}

/// Local request and guard tests.
///
/// Host per-CPU storage verifies balanced nesting and one-shot pending consumption.
#[cfg(test)]
mod tests {
    use super::{
        can_schedule_now, disable_preempt, enable_preempt, request_reschedule,
        take_reschedule_request,
    };

    /// Consumes one request exactly once.
    ///
    /// Repeated safe-point observations cannot invent pending work.
    #[test]
    fn pending_request_is_one_shot() {
        request_reschedule();
        assert!(take_reschedule_request());
        assert!(!take_reschedule_request());
    }

    /// Restores nesting after a balanced critical section.
    ///
    /// Scheduling remains disallowed while the guard depth is nonzero.
    #[test]
    fn preemption_depth_is_balanced() {
        disable_preempt();
        assert!(!can_schedule_now());
        enable_preempt();
    }
    /// Preserves pending intent when interrupt-exit policy defers consumption.
    ///
    /// A later permitted safe point consumes the exact same request once.
    #[test]
    fn deferred_request_remains_pending() {
        request_reschedule();
        assert!(!super::take_reschedule_request_if(false));
        assert!(super::take_reschedule_request_if(true));
        assert!(!super::take_reschedule_request_if(true));
    }
}
