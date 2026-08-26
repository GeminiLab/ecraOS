//! Preemption state and deferred scheduling requests.
//!
//! This module keeps the scheduler's interrupt-safe preemption contract independent from the
//! architecture trap code. Stage 2B intentionally has no IPI, so remote requests are observed on
//! the next local timer interrupt.

use core::sync::atomic::{AtomicBool, Ordering};

use expercpu::def_percpu;

/// Per-CPU preemption nesting depth.
#[def_percpu]
static PREEMPT_DEPTH: usize = 0;

/// Per-CPU deferred scheduling request.
#[def_percpu]
static RESCHEDULE_PENDING: AtomicBool = AtomicBool::new(false);

/// A guard that defers preemption until dropped.
pub struct RescheduleGuard {
    inner: kernel_guard::NoPreempt,
}

impl RescheduleGuard {
    /// Disables preemption on the current CPU.
    pub fn new() -> Self {
        Self {
            inner: kernel_guard::NoPreempt::new(),
        }
    }
}

impl Default for RescheduleGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RescheduleGuard {
    /// Re-enables preemption and preserves any deferred request.
    fn drop(&mut self) {
        let _ = &self.inner;
    }
}

/// Disables preemption for the current CPU through the `kernel_guard` bridge.
pub fn disable_preempt() {
    // SAFETY: KernelGuardIf invokes this before a guard can permit scheduler migration. Raw
    // per-CPU access avoids recursively acquiring NoPreempt through generated helpers.
    unsafe {
        let depth = PREEMPT_DEPTH.current_ref_mut_raw();
        *depth = depth.checked_add(1).expect("preemption depth overflow");
    }
}

/// Enables preemption for the current CPU through the `kernel_guard` bridge.
pub fn enable_preempt() {
    // SAFETY: KernelGuardIf invokes this while its matching NoPreempt guard is being released.
    // Raw access avoids recursively reacquiring the same guard.
    unsafe {
        let depth = PREEMPT_DEPTH.current_ref_mut_raw();
        *depth = depth.checked_sub(1).expect("preemption depth underflow");
    }
}

/// Returns whether a scheduler context switch is currently permitted.
pub fn can_schedule_now() -> bool {
    // SAFETY: This is a diagnostic safe-point check and callers already serialize task state.
    (unsafe { *PREEMPT_DEPTH.current_ref_raw() == 0 }) && exarch::irq::local_enabled()
}

/// Requests a local reschedule at the next safe point.
pub fn request_reschedule() {
    // SAFETY: Queue publication precedes this Release store. The per-CPU bit may be published by
    // a remote wake path through the scheduler's synchronized remote reference in a later task.
    unsafe {
        RESCHEDULE_PENDING
            .current_ref_raw()
            .store(true, Ordering::Release)
    }
}

/// Accounts one timer tick for the current task and defers preemption when its slice expires.
pub fn timer_tick() {
    let Some(task) = super::scheduler::current_task_ref() else {
        return;
    };
    if task.account_tick() {
        request_reschedule();
    }
}

/// Consumes one pending request with acquire-release ordering.
pub fn take_reschedule_request() -> bool {
    // SAFETY: The local safe point consumes this CPU's request with AcqRel ordering, so a new
    // publisher after the swap leaves the bit set for the following safe point.
    unsafe {
        RESCHEDULE_PENDING
            .current_ref_raw()
            .swap(false, Ordering::AcqRel)
    }
}

/// Returns the current preemption nesting depth for diagnostics and tests.
pub fn depth() -> usize {
    // SAFETY: Callers use this only as a local diagnostic snapshot.
    unsafe { *PREEMPT_DEPTH.current_ref_raw() }
}

#[cfg(test)]
mod tests {
    use super::{
        can_schedule_now, disable_preempt, enable_preempt, request_reschedule,
        take_reschedule_request,
    };

    #[test]
    fn pending_request_is_one_shot() {
        request_reschedule();
        assert!(take_reschedule_request());
        assert!(!take_reschedule_request());
    }

    #[test]
    fn preemption_depth_is_balanced() {
        disable_preempt();
        assert!(!can_schedule_now());
        enable_preempt();
    }
}
