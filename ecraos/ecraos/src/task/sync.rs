//! FIFO synchronization primitives for kernel tasks.
//!
//! These primitives keep registration and wake publication separate from scheduler switching.

use alloc::collections::VecDeque;
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use kspin::SpinNoIrq;

use super::{TaskError, TaskRef, WaitResult};

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
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        task.claim_waiting()?;
        self.queue.lock().push_back(task);
        Ok(())
    }

    /// Atomically registers and blocks the current task while holding the wait-queue lock.
    ///
    /// Scheduler code calls this helper with local interrupts disabled. The queue lock covers the
    /// lifecycle transition, so a concurrent wake cannot observe a published-but-running task.
    pub(crate) fn prepare_block(&self, task: TaskRef, cpu_id: usize) -> Result<(), TaskError> {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue locks by CpuId -> task
        // lifecycle -> completion or wait-queue locks. A future lock-free wait queue may replace
        // these locks after a complete memory-ordering proof.
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let mut queue = self.queue.lock();
        task.claim_waiting()?;
        if task.block(cpu_id).is_err() {
            task.release_waiting();
            return Err(TaskError::CannotBlock);
        }
        queue.push_back(task);
        Ok(())
    }

    /// Atomically registers a timed waiter and commits its `TimedWaiting` state.
    pub(crate) fn prepare_timed_block(
        &self,
        task: TaskRef,
        cpu_id: usize,
    ) -> Result<u64, TaskError> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
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
    pub fn wake_one(&self) -> Option<TaskRef> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
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
            super::scheduler::wake_waiter(task.clone());
            return Some(task);
        }
    }

    /// Wakes and removes every waiter in FIFO order without switching contexts.
    pub fn wake_all(&self) -> alloc::vec::Vec<TaskRef> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let tasks = self.queue.lock().drain(..).collect::<alloc::vec::Vec<_>>();
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
                super::scheduler::wake_waiter(task.clone());
            }
        }
        tasks
    }

    /// Removes the oldest waiter without changing its lifecycle state.
    pub(crate) fn take_one(&self) -> Option<TaskRef> {
        // Lock order is NoPreemptIrqSave -> domain metadata -> Queue -> task lifecycle ->
        // completion/wait queue. The caller performs wake publication after releasing this lock.
        // A future lock-free wait queue may replace this lock after its memory-ordering proof.
        let mut queue = self.queue.lock();
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
    pub(crate) fn remove_task(&self, task_id: u64) -> Option<TaskRef> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
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

/// A non-reentrant blocking mutex.
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
        let caller = super::scheduler::current_task_id().unwrap_or(0) as usize;
        if self.owner.load(Ordering::Acquire) == caller && caller != 0 {
            return Err(TaskError::WouldDeadlock);
        }
        let reserved = self.reserved.load(Ordering::Acquire);
        if (reserved != 0 && reserved != caller) || (!self.waiters.is_empty() && reserved == 0) {
            return Err(TaskError::CannotBlock);
        }
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let mut value = self.value.lock();
        let Some(inner) = value.take() else {
            return Err(TaskError::CannotBlock);
        };
        self.owner.store(caller, Ordering::Release);
        if reserved == caller {
            self.reserved.store(0, Ordering::Release);
        }
        Ok(MutexGuard {
            mutex: self,
            value: Some(inner),
        })
    }

    /// Acquires the mutex, blocking in FIFO order while another task owns it.
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, TaskError> {
        if super::scheduler::current_task_id().is_none() {
            return Err(TaskError::NotTaskContext);
        }
        loop {
            match self.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TaskError::CannotBlock) => {
                    super::scheduler::block_current_on_wait_queue(&self.waiters)?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Returns the number of queued waiters.
    pub fn waiter_count(&self) -> usize {
        self.waiters.len()
    }

    fn unlock(&self, value: T) {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        *self.value.lock() = Some(value);
        self.owner.store(0, Ordering::Release);
        if let Some(task) = self.waiters.take_one() {
            self.reserved.store(task.id() as usize, Ordering::Release);
            task.release_waiting();
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

/// A bounded single-permit semaphore.
pub struct Semaphore {
    /// Number of currently available permits.
    permits: AtomicUsize,
    /// Maximum permit count.
    max: usize,
    /// FIFO waiters blocked while no permit is available.
    waiters: WaitQueue,
    /// Task ID receiving the next released permit, or zero when no permit is reserved.
    reserved: AtomicUsize,
}

impl Semaphore {
    /// Creates a semaphore with `initial` permits and a fixed `max`.
    pub const fn new(initial: usize, max: usize) -> Self {
        assert!(initial <= max);
        Self {
            permits: AtomicUsize::new(initial),
            max,
            waiters: WaitQueue::new(),
            reserved: AtomicUsize::new(0),
        }
    }

    /// Acquires one permit, blocking in FIFO order while none is available.
    pub fn acquire(&self) -> Result<(), TaskError> {
        let caller = super::scheduler::current_task_id().ok_or(TaskError::NotTaskContext)?;
        loop {
            let current = self.permits.load(Ordering::Acquire);
            let reserved = self.reserved.load(Ordering::Acquire);
            if reserved == caller as usize {
                self.reserved.store(0, Ordering::Release);
                return Ok(());
            }
            if current != 0 && self.waiters.is_empty() {
                if self
                    .permits
                    .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return Ok(());
                }
                continue;
            }
            super::scheduler::block_current_on_wait_queue(&self.waiters)?;
        }
    }

    /// Releases one permit and rejects over-release.
    pub fn release(&self) -> Result<(), TaskError> {
        if let Some(task) = self.waiters.take_one() {
            self.reserved.store(task.id() as usize, Ordering::Release);
            task.release_waiting();
            super::scheduler::wake_waiter(task);
            return Ok(());
        }
        let mut current = self.permits.load(Ordering::Acquire);
        loop {
            if current >= self.max {
                return Err(TaskError::InvalidTarget);
            }
            match self.permits.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    /// Returns the number of available permits.
    pub fn available(&self) -> usize {
        self.permits.load(Ordering::Acquire)
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
