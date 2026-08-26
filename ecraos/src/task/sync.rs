//! FIFO synchronization primitives for kernel tasks.
//!
//! These primitives keep registration and wake publication separate from scheduler switching.

use alloc::collections::VecDeque;
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use kspin::SpinNoIrq;

use super::{TaskError, TaskRef};

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

    /// Wakes and removes the oldest waiter without switching contexts.
    pub fn wake_one(&self) -> Option<TaskRef> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let task = self.queue.lock().pop_front()?;
        task.release_waiting();
        Some(task)
    }

    /// Wakes and removes every waiter in FIFO order without switching contexts.
    pub fn wake_all(&self) -> alloc::vec::Vec<TaskRef> {
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let tasks = self.queue.lock().drain(..).collect::<alloc::vec::Vec<_>>();
        for task in &tasks {
            task.release_waiting();
        }
        tasks
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
}

impl<T> Mutex<T> {
    /// Creates an unlocked mutex containing `value`.
    pub const fn new(value: T) -> Self {
        Self {
            value: SpinNoIrq::new(Some(value)),
            owner: AtomicUsize::new(0),
            waiters: WaitQueue::new(),
        }
    }

    /// Attempts to acquire the mutex without blocking.
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, TaskError> {
        let caller = super::scheduler::current_task_id().unwrap_or(0) as usize;
        if self.owner.load(Ordering::Acquire) == caller && caller != 0 {
            return Err(TaskError::WouldDeadlock);
        }
        if !self.waiters.is_empty() {
            return Err(TaskError::CannotBlock);
        }
        let _guard = kernel_guard::NoPreemptIrqSave::new();
        let mut value = self.value.lock();
        let Some(inner) = value.take() else {
            return Err(TaskError::CannotBlock);
        };
        self.owner.store(caller, Ordering::Release);
        Ok(MutexGuard {
            mutex: self,
            value: Some(inner),
        })
    }

    /// Acquires the mutex, yielding while another task owns it.
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, TaskError> {
        let task = super::scheduler::current_task_ref().ok_or(TaskError::NotTaskContext)?;
        let mut queued = false;
        loop {
            match self.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TaskError::CannotBlock) => {
                    if !queued {
                        self.waiters.enqueue(task.clone())?;
                        queued = true;
                    }
                    super::scheduler::yield_now();
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
        let _ = self.waiters.wake_one();
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
    /// Notification sequence used to distinguish notifications.
    sequence: AtomicUsize,
}

impl Condvar {
    /// Creates an empty condition variable.
    pub const fn new() -> Self {
        Self {
            sequence: AtomicUsize::new(0),
        }
    }

    /// Waits by releasing the supplied mutex and yielding once before reacquiring it.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> Result<MutexGuard<'a, T>, TaskError> {
        let mutex = guard.mutex;
        drop(guard);
        super::scheduler::yield_now();
        mutex.lock()
    }

    /// Notifies one waiter at the next predicate check.
    pub fn notify_one(&self) {
        self.sequence.fetch_add(1, Ordering::Release);
    }

    /// Notifies all waiters at the next predicate check.
    pub fn notify_all(&self) {
        self.sequence.fetch_add(1, Ordering::Release);
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
}

impl Semaphore {
    /// Creates a semaphore with `initial` permits and a fixed `max`.
    pub const fn new(initial: usize, max: usize) -> Self {
        assert!(initial <= max);
        Self {
            permits: AtomicUsize::new(initial),
            max,
        }
    }

    /// Acquires one permit, yielding while none is available.
    pub fn acquire(&self) -> Result<(), TaskError> {
        loop {
            let current = self.permits.load(Ordering::Acquire);
            if current != 0 {
                if self
                    .permits
                    .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return Ok(());
                }
                continue;
            }
            if super::scheduler::current_task_id().is_none() {
                return Err(TaskError::NotTaskContext);
            }
            super::scheduler::yield_now();
        }
    }

    /// Releases one permit and rejects over-release.
    pub fn release(&self) -> Result<(), TaskError> {
        let current = self.permits.load(Ordering::Acquire);
        if current >= self.max {
            return Err(TaskError::InvalidTarget);
        }
        self.permits.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Returns the number of available permits.
    pub fn available(&self) -> usize {
        self.permits.load(Ordering::Acquire)
    }
}
