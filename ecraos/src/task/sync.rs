//! FIFO synchronization primitives for kernel tasks.
//!
//! These primitives keep registration and wake publication separate from scheduler switching.

use alloc::{collections::VecDeque, sync::Arc};
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use exspin::SpinNoIrq;

use super::{TaskError, TaskId, TaskRef, WaitResult};
use crate::mp::LogicalCpuId;

/// One scheduling gap armed only by the deterministic foundation workload.
#[cfg(feature = "foundation-test")]
#[expercpu::def_percpu]
static YIELD_BEFORE_WAIT: bool = false;

/// Arms one yield after a failed acquisition and before waiter registration on this CPU.
#[cfg(feature = "foundation-test")]
pub(crate) fn yield_before_next_sync_wait() {
    YIELD_BEFORE_WAIT.write_current(true);
}

/// Runs an armed scheduling gap before acquiring any synchronization queue lock.
#[cfg(feature = "foundation-test")]
pub(super) fn run_wait_registration_hook() {
    if YIELD_BEFORE_WAIT.read_current() {
        YIELD_BEFORE_WAIT.write_current(false);
        super::scheduler::yield_now();
    }
}

/// A predicate-free FIFO task wait queue.
pub struct WaitQueue {
    /// Registered task references in FIFO order.
    queue: SpinNoIrq<VecDeque<TaskRef>>,
}

impl WaitQueue {
    /// Creates an empty wait queue.
    pub const fn new() -> Self {
        Self {
            queue: SpinNoIrq::new(VecDeque::new()),
        }
    }

    /// Registers one task at the FIFO tail.
    pub fn enqueue(&self, task: TaskRef) -> Result<(), TaskError> {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue -> task lifecycle ->
        // completion/wait queue. A future lock-free wait queue may replace this lock after its
        // memory-ordering proof.
        let _guard = exspin::NoPreemptIrqSave::new();
        task.claim_waiting()?;
        self.queue.lock().push_back(task);
        Ok(())
    }

    /// Atomically registers and blocks the current task while holding the wait-queue lock.
    ///
    /// Scheduler code calls this helper with local interrupts disabled. The queue lock covers the
    /// lifecycle transition, so a concurrent wake cannot observe a published-but-running task.
    pub(crate) fn prepare_block(
        &self,
        task: TaskRef,
        cpu_id: LogicalCpuId,
    ) -> Result<(), TaskError> {
        self.prepare_block_if(task, cpu_id, |_| true).map(|_| ())
    }

    /// Rechecks a resource predicate and registers a blocked waiter under the same queue lock.
    ///
    /// Returns false without changing task state when `should_block` is false. The predicate must
    /// not block or reacquire this queue. Resource acquisition and release must use this queue
    /// lock too, so neither can pass between the predicate check and waiter publication.
    pub(crate) fn prepare_block_if(
        &self,
        task: TaskRef,
        cpu_id: LogicalCpuId,
        should_block: impl FnOnce(&VecDeque<TaskRef>) -> bool,
    ) -> Result<bool, TaskError> {
        let mut queue = self.queue.lock();
        if !should_block(&queue) {
            return Ok(false);
        }
        task.claim_waiting()?;
        if task.block(cpu_id).is_err() {
            task.release_waiting();
            return Err(TaskError::CannotBlock);
        }
        queue.push_back(task);
        Ok(true)
    }

    /// Atomically registers a timed waiter and commits its `TimedWaiting` state.
    pub(crate) fn prepare_timed_block(
        &self,
        task: TaskRef,
        cpu_id: LogicalCpuId,
    ) -> Result<u64, TaskError> {
        let _guard = exspin::NoPreemptIrqSave::new();
        let mut queue = self.queue.lock();
        task.claim_waiting()?;
        let generation = match task.begin_timed_wait(cpu_id) {
            Ok(generation) => generation,
            Err(_) => {
                task.release_waiting();
                return Err(TaskError::CannotBlock);
            }
        };
        task.set_wait_queue(self as *const Self as *const ());
        queue.push_back(task);
        Ok(generation)
    }

    /// Wakes and removes the oldest waiter without switching contexts.
    ///
    /// Queue and timer bookkeeping finish before remote scheduler notification.
    pub fn wake_one(&self) -> Option<TaskRef> {
        let _guard = exspin::NoPreemptIrqSave::new();
        loop {
            let task = self.take_one()?;
            if !matches!(
                task.state(),
                super::TaskState::Blocked | super::TaskState::TimedWaiting
            ) {
                task.release_waiting();
                continue;
            }
            let timed = matches!(task.state(), super::TaskState::TimedWaiting);
            let timer_cpu = task.timer_cpu();
            let generation = task.sleep_generation();
            task.release_waiting();
            task.take_wait_queue();
            if timed {
                if let Some(cpu_id) = timer_cpu {
                    super::scheduler::cancel_timed_wait(&task, generation, cpu_id);
                }
            }
            drop(_guard);
            super::scheduler::wake_waiter(task.clone());
            return Some(task);
        }
    }

    /// Wakes and removes every waiter in FIFO order without switching contexts.
    ///
    /// Selected ownership is retained while IRQ-saving bookkeeping ends before queue publication.
    pub fn wake_all(&self) -> alloc::vec::Vec<TaskRef> {
        let _guard = exspin::NoPreemptIrqSave::new();
        let tasks = self.queue.lock().drain(..).collect::<alloc::vec::Vec<_>>();
        let mut selected = alloc::vec::Vec::with_capacity(tasks.len());
        for task in &tasks {
            task.release_waiting();
            if matches!(
                task.state(),
                super::TaskState::Blocked | super::TaskState::TimedWaiting
            ) {
                let timed = matches!(task.state(), super::TaskState::TimedWaiting);
                let timer_cpu = task.timer_cpu();
                let generation = task.sleep_generation();
                task.take_wait_queue();
                if timed {
                    if let Some(cpu_id) = timer_cpu {
                        super::scheduler::cancel_timed_wait(task, generation, cpu_id);
                    }
                }
                selected.push(task.clone());
            }
        }
        drop(_guard);
        for task in selected {
            super::scheduler::wake_waiter(task);
        }
        tasks
    }

    /// Removes the oldest waiter without changing its lifecycle state.
    pub(crate) fn take_one(&self) -> Option<TaskRef> {
        Self::take_one_locked(&mut self.queue.lock())
    }

    /// Removes the oldest blocked waiter while the caller retains the queue lock.
    ///
    /// Resource release may update ownership before dropping this lock and publishing the wake.
    fn take_one_locked(queue: &mut VecDeque<TaskRef>) -> Option<TaskRef> {
        loop {
            let task = queue.pop_front()?;
            if matches!(
                task.state(),
                super::TaskState::Blocked | super::TaskState::TimedWaiting
            ) {
                return Some(task);
            }
            task.release_waiting();
        }
    }

    /// Removes one exact waiter during timeout processing.
    pub(crate) fn remove_task(&self, task_id: TaskId) -> Option<TaskRef> {
        let _guard = exspin::NoPreemptIrqSave::new();
        let mut queue = self.queue.lock();
        let index = queue.iter().position(|task| task.id() == task_id)?;
        queue.remove(index)
    }

    /// Returns the number of registered waiters.
    pub fn len(&self) -> usize {
        let _guard = kernel_guard::NoPreempt::new();
        self.queue.lock().len()
    }

    /// Returns whether no task is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A non-reentrant blocking mutex with FIFO ownership handoff.
///
/// The wait-queue lock serializes admission, waiter registration, value return and reservation.
/// No lock is retained across a scheduler context switch or wake publication.
pub struct Mutex<T> {
    /// Protected value and ownership state.
    value: SpinNoIrq<Option<T>>,
    /// Current owner task identity, or zero outside task context.
    owner: AtomicUsize,
    /// FIFO waiters used for admission ordering.
    waiters: WaitQueue,
    /// Task ID reserved by unlock for the next FIFO waiter, or zero when no handoff is pending.
    reserved: AtomicUsize,
}

impl<T> Mutex<T> {
    /// Creates an unlocked mutex containing `value`.
    pub const fn new(value: T) -> Self {
        Self {
            value: SpinNoIrq::new(Some(value)),
            owner: AtomicUsize::new(0),
            waiters: WaitQueue::new(),
            reserved: AtomicUsize::new(0),
        }
    }

    /// Attempts to acquire the mutex without blocking.
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, TaskError> {
        let caller = super::scheduler::current_task_id().map_or(0, |id| id.into_repr() as usize);
        let queue = self.waiters.queue.lock();
        if self.owner.load(Ordering::Acquire) == caller && caller != 0 {
            return Err(TaskError::WouldDeadlock);
        }
        if self.admission_blocked(caller, &queue) {
            return Err(TaskError::CannotBlock);
        }
        let mut value = self.value.lock();
        let Some(inner) = value.take() else {
            return Err(TaskError::CannotBlock);
        };
        self.owner.store(caller, Ordering::Release);
        if self.reserved.load(Ordering::Acquire) == caller {
            self.reserved.store(0, Ordering::Release);
        }
        Ok(MutexGuard {
            mutex: self,
            value: Some(inner),
        })
    }

    /// Acquires the mutex, blocking in FIFO order while another task owns it.
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, TaskError> {
        let caller = super::scheduler::current_task_id()
            .ok_or(TaskError::NotTaskContext)?
            .into_repr() as usize;
        loop {
            match self.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TaskError::CannotBlock) => {
                    super::scheduler::block_current_if(|task, cpu_id| {
                        self.waiters.prepare_block_if(task, cpu_id, |queue| {
                            self.admission_blocked(caller, queue)
                        })
                    })?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Returns the number of queued waiters.
    pub fn waiter_count(&self) -> usize {
        self.waiters.len()
    }

    /// Tests FIFO admission and value availability while the caller holds this mutex's queue.
    fn admission_blocked(&self, caller: usize, queue: &VecDeque<TaskRef>) -> bool {
        let reserved = self.reserved.load(Ordering::Acquire);
        (reserved != 0 && reserved != caller)
            || (!queue.is_empty() && reserved == 0)
            || self.value.lock().is_none()
    }

    /// Returns the value and reserves FIFO ownership before waking a waiter.
    ///
    /// Remote queue notification follows release of the IRQ-saving publication guard.
    fn unlock(&self, value: T) {
        let waiter = {
            let mut queue = self.waiters.queue.lock();
            *self.value.lock() = Some(value);
            self.owner.store(0, Ordering::Release);
            let waiter = WaitQueue::take_one_locked(&mut queue);
            self.reserved.store(
                waiter
                    .as_ref()
                    .map_or(0, |task| task.id().into_repr() as usize),
                Ordering::Release,
            );
            if let Some(task) = &waiter {
                task.release_waiting();
            }
            waiter
        };
        if let Some(task) = waiter {
            super::scheduler::wake_waiter(task);
        }
    }
}

/// A guard holding one mutex value.
pub struct MutexGuard<'a, T> {
    /// The mutex that owns the protected value.
    mutex: &'a Mutex<T>,
    /// Value temporarily moved out of the mutex storage.
    value: Option<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("mutex guard value missing")
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("mutex guard value missing")
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.mutex.unlock(value);
        }
    }
}

/// A condition variable associated with one mutex.
pub struct Condvar {
    /// FIFO waiters registered before the associated mutex is released.
    waiters: WaitQueue,
}

impl Condvar {
    /// Creates an empty condition variable.
    pub const fn new() -> Self {
        Self {
            waiters: WaitQueue::new(),
        }
    }

    /// Releases the supplied mutex, blocks, and reacquires it after a notification.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> Result<MutexGuard<'a, T>, TaskError> {
        let mutex = guard.mutex;
        let task = super::scheduler::current_task_ref().ok_or(TaskError::NotTaskContext)?;
        let cpu_id = super::scheduler::current_cpu_id();
        let irq_enabled = exarch::trap::local_enabled();
        exarch::trap::disable_local();
        if let Err(error) = self.waiters.prepare_block(task, cpu_id) {
            if irq_enabled {
                exarch::trap::enable_local();
            }
            return Err(error);
        }
        drop(guard);
        let result = super::scheduler::block_current_registered();
        if irq_enabled {
            exarch::trap::enable_local();
        }
        result?;
        mutex.lock()
    }

    /// Waits until notification or the relative deadline, then reacquires the mutex.
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: core::time::Duration,
    ) -> Result<(MutexGuard<'a, T>, WaitResult), TaskError> {
        let mutex = guard.mutex;
        let task = super::scheduler::current_task_ref().ok_or(TaskError::NotTaskContext)?;
        let cpu_id = super::scheduler::current_cpu_id();
        let deadline = exarch::time::monotonic_time().saturating_add(timeout);
        let irq_enabled = exarch::trap::local_enabled();
        exarch::trap::disable_local();
        let generation = match self.waiters.prepare_timed_block(task.clone(), cpu_id) {
            Ok(generation) => generation,
            Err(error) => {
                if irq_enabled {
                    exarch::trap::enable_local();
                }
                return Err(error);
            }
        };
        drop(guard);
        if let Err(error) =
            super::scheduler::register_timed_wait(task.clone(), generation, deadline, cpu_id)
        {
            self.waiters.remove_task(task.id());
            task.cancel_timed_wait(generation, cpu_id);
            task.release_waiting();
            if irq_enabled {
                exarch::trap::enable_local();
            }
            return Err(error);
        }
        if let Err(error) = super::scheduler::block_current_registered() {
            self.waiters.remove_task(task.id());
            super::scheduler::cancel_timed_wait(&task, generation, cpu_id);
            task.release_waiting();
            if irq_enabled {
                exarch::trap::enable_local();
            }
            return Err(error);
        }
        if irq_enabled {
            exarch::trap::enable_local();
        }
        let result = task.take_wait_result().unwrap_or(WaitResult::Woken);
        Ok((mutex.lock()?, result))
    }

    /// Wakes one FIFO waiter without transferring mutex ownership.
    pub fn notify_one(&self) {
        self.waiters.wake_one();
    }

    /// Wakes all FIFO waiters without transferring mutex ownership.
    pub fn notify_all(&self) {
        self.waiters.wake_all();
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

/// A bounded FIFO counting semaphore with one permit per acquisition.
///
/// The wait-queue lock serializes permit consumption, waiter registration and release. A release
/// either increments the available count or transfers one permit directly to the selected waiter.
/// Each waiter retains its own grant, so unrelated wakes do not complete an acquisition and later
/// releases cannot overwrite permits already assigned to other waiters.
pub struct Semaphore {
    /// Number of currently available permits.
    permits: AtomicUsize,
    /// Maximum permit count.
    max: usize,
    /// FIFO acquisition records, retained across wakes that do not supply a permit.
    waiters: SpinNoIrq<VecDeque<Arc<SemaphoreWaiter>>>,
}

/// One semaphore acquisition and its independent, monotonic permit grant.
///
/// The acquiring task retains this record until success. The release path removes its queue
/// reference and publishes `granted` before wakeup. An unrelated wake preserves queue membership
/// and cannot consume another acquisition's grant, even when the same task waits again later.
struct SemaphoreWaiter {
    /// The task whose acquisition owns this queue position.
    task: TaskRef,
    /// Whether one release has transferred its permit to this exact acquisition.
    granted: AtomicBool,
}

impl Semaphore {
    /// Creates a semaphore with `initial` permits and a fixed `max`.
    pub const fn new(initial: usize, max: usize) -> Self {
        assert!(initial <= max);
        Self {
            permits: AtomicUsize::new(initial),
            max,
            waiters: SpinNoIrq::new(VecDeque::new()),
        }
    }

    /// Acquires one permit, blocking in FIFO order while none is available.
    ///
    /// A blocked acquisition consumes only its own release-side grant. An unrelated wake rechecks
    /// that grant and blocks again without losing its FIFO position.
    pub fn acquire(&self) -> Result<(), TaskError> {
        super::scheduler::current_task_id().ok_or(TaskError::NotTaskContext)?;
        let mut waiter = None;
        loop {
            if self.try_acquire_permit(waiter.as_deref()) {
                return Ok(());
            }
            super::scheduler::block_current_if(|task, cpu_id| {
                self.prepare_acquire(&mut waiter, task, cpu_id)
            })?;
        }
    }

    /// Releases one permit and rejects over-release.
    pub fn release(&self) -> Result<(), TaskError> {
        let waiter = {
            let mut queue = self.waiters.lock();
            let waiter = queue.pop_front();
            if let Some(waiter) = &waiter {
                waiter.granted.store(true, Ordering::Release);
                waiter.task.release_waiting();
            } else {
                let current = self.permits.load(Ordering::Acquire);
                if current >= self.max {
                    return Err(TaskError::InvalidTarget);
                }
                self.permits.store(current + 1, Ordering::Release);
            }
            waiter
        };
        if let Some(waiter) = waiter {
            super::scheduler::wake_waiter(Arc::clone(&waiter.task));
        }
        Ok(())
    }

    /// Returns the number of available permits.
    pub fn available(&self) -> usize {
        self.permits.load(Ordering::Acquire)
    }

    /// Consumes an unassigned permit while excluding waiter registration and release.
    fn try_acquire_permit(&self, waiter: Option<&SemaphoreWaiter>) -> bool {
        let queue = self.waiters.lock();
        if waiter.is_some_and(|waiter| waiter.granted.load(Ordering::Acquire)) {
            return true;
        }
        let current = self.permits.load(Ordering::Acquire);
        if current == 0 || !queue.is_empty() {
            return false;
        }
        self.permits.store(current - 1, Ordering::Release);
        true
    }

    /// Rechecks admission and commits or renews this acquisition's wait under the resource lock.
    ///
    /// A waiter resumed without a grant still owns its existing queue entry and registration. It
    /// may commit `Running -> Blocked` again without duplicating either entry or queue position.
    fn prepare_acquire(
        &self,
        waiter: &mut Option<Arc<SemaphoreWaiter>>,
        task: TaskRef,
        cpu_id: LogicalCpuId,
    ) -> Result<bool, TaskError> {
        let mut queue = self.waiters.lock();
        if waiter
            .as_ref()
            .is_some_and(|waiter| waiter.granted.load(Ordering::Acquire))
            || (self.permits.load(Ordering::Acquire) != 0 && queue.is_empty())
        {
            return Ok(false);
        }
        let waiter = waiter.get_or_insert_with(|| {
            Arc::new(SemaphoreWaiter {
                task,
                granted: AtomicBool::new(false),
            })
        });
        let position = queue.iter().position(|entry| Arc::ptr_eq(entry, waiter));
        match waiter.task.claim_waiting() {
            Ok(()) => {}
            Err(TaskError::AlreadyWaiting) if position.is_some() => {}
            Err(error) => return Err(error),
        }
        if waiter.task.block(cpu_id).is_err() {
            if let Some(position) = position {
                queue.remove(position);
            }
            waiter.task.release_waiting();
            return Err(TaskError::CannotBlock);
        }
        if position.is_none() {
            queue.push_back(Arc::clone(waiter));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::Semaphore;
    use crate::task::TaskError;

    #[test]
    fn semaphore_release_never_exceeds_its_bound() {
        let semaphore = Semaphore::new(0, 1);
        assert_eq!(semaphore.release(), Ok(()));
        assert_eq!(semaphore.available(), 1);
        assert_eq!(semaphore.release(), Err(TaskError::InvalidTarget));
        assert_eq!(semaphore.available(), 1);
    }
}
