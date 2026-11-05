//! Reader-writer spin locks with kernel execution guards.
//!
//! Uses the reader-count and writer-bit representation from spin's reader-writer lock design.
//! Successful acquisitions use acquire ordering and unlocking uses release ordering. There is
//! no fairness guarantee, so a steady stream of readers may starve a writer.
//!
//! Unlike the mutex, this lock retains its atomic state without the smp feature. Shared guards
//! can coexist even on one CPU, so exclusive access must still check the active reader count.

use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use kernel_guard::BaseGuard;
use kspin::{BaseSpinLock, BaseSpinLockGuard};

use crate::irq::NoIrqGuardPolicy;

#[cfg(test)]
mod tests;

/// The exclusive-writer bit in the lock state.
const WRITER: usize = 1;
/// The amount contributed to the state by each shared reader.
const READER: usize = 2;

/// A spin lock allowing shared readers or one exclusive writer.
///
/// The execution guard G is acquired before waiting for either kind of access and released after
/// unlocking. A failed try_read or try_write restores the guard immediately. No allocation or
/// poisoning is used. Recursive acquisition and read-to-write upgrading are not supported.
pub struct BaseRwLock<G: BaseGuard, T: ?Sized> {
    /// The selected execution-guard policy.
    _policy: PhantomData<G>,
    /// The reader count shifted left once, or WRITER for an exclusive holder.
    state: AtomicUsize,
    /// The payload, accessed only under the corresponding read or write permission.
    data: UnsafeCell<T>,
}

/// An execution guard restored on its originating CPU after the data lock is released.
struct ExecutionGuard<G: BaseGuard> {
    /// The execution state captured before attempting acquisition.
    state: G::State,
    /// Prevents moving or sharing a CPU-local execution guard between threads.
    _local: PhantomData<(*mut (), G)>,
}

/// Shared data access and its CPU-local execution guard.
///
/// Dropping the guard releases its reader slot before restoring execution state. Guards cannot
/// move to another thread because interrupt and preemption state belongs to the acquiring CPU.
///
/// ```compile_fail
/// let lock = exspin::RwLockNoIrq::new(0);
/// let guard = lock.read();
/// std::thread::scope(|scope| {
///     scope.spawn(move || drop(guard));
/// });
/// ```
pub struct BaseRwLockReadGuard<'a, G: BaseGuard, T: ?Sized> {
    /// The lock whose reader count includes this guard.
    lock: &'a BaseRwLock<G, T>,
    /// Restores execution state after the Drop body unlocks the data.
    _execution: ExecutionGuard<G>,
}

/// Exclusive data access and its CPU-local execution guard.
///
/// Dropping the guard releases write access before restoring execution state. The guard must
/// remain on the acquiring CPU.
///
/// ```compile_fail
/// let lock = exspin::RwLockNoIrq::new(0);
/// let guard = lock.write();
/// std::thread::scope(|scope| {
///     scope.spawn(move || drop(guard));
/// });
/// ```
pub struct BaseRwLockWriteGuard<'a, G: BaseGuard, T: ?Sized> {
    /// The lock exclusively held by this guard.
    lock: &'a BaseRwLock<G, T>,
    /// Restores execution state after the Drop body unlocks the data.
    _execution: ExecutionGuard<G>,
}

// SAFETY: Moving a lock transfers ownership of T. No execution state is stored until acquisition.
unsafe impl<G: BaseGuard, T: ?Sized + Send> Send for BaseRwLock<G, T> {}
// SAFETY: Writers require exclusive access and T: Send. Concurrent readers require T: Sync.
unsafe impl<G: BaseGuard, T: ?Sized + Send + Sync> Sync for BaseRwLock<G, T> {}

/// Execution-state acquisition and automatic restoration.
impl<G: BaseGuard> ExecutionGuard<G> {
    /// Saves and protects execution state before attempting to acquire the lock.
    #[inline]
    fn acquire() -> Self {
        Self {
            state: G::acquire(),
            _local: PhantomData,
        }
    }
}

/// Restoration for failed acquisitions, successful guards, and panic unwinding.
impl<G: BaseGuard> Drop for ExecutionGuard<G> {
    /// Restores the execution state on the thread that acquired it.
    #[inline]
    fn drop(&mut self) {
        G::release(self.state);
    }
}

/// Construction and owned access to a reader-writer lock.
impl<G: BaseGuard, T> BaseRwLock<G, T> {
    /// Creates an unlocked reader-writer lock with the selected execution policy.
    pub const fn new(data: T) -> Self {
        Self {
            _policy: PhantomData,
            state: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes the lock and returns its data without acquiring an execution guard.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

/// Shared and exclusive acquisition with execution-state protection.
impl<G: BaseGuard, T: ?Sized> BaseRwLock<G, T> {
    /// Acquires shared access while protecting execution state throughout the wait.
    ///
    /// No fairness or recursive-acquisition guarantee is provided.
    ///
    /// # Panics
    ///
    /// Panics before the reader count would overflow. Execution state is restored on unwind.
    #[inline]
    pub fn read(&self) -> BaseRwLockReadGuard<'_, G, T> {
        let execution = ExecutionGuard::acquire();
        while !self.acquire_reader() {
            core::hint::spin_loop();
        }
        BaseRwLockReadGuard {
            lock: self,
            _execution: execution,
        }
    }

    /// Attempts shared access without waiting for an existing writer.
    ///
    /// Returns None and restores execution state when a writer holds the lock.
    ///
    /// # Panics
    ///
    /// Panics before the reader count would overflow. Execution state is restored on unwind.
    #[inline]
    pub fn try_read(&self) -> Option<BaseRwLockReadGuard<'_, G, T>> {
        let execution = ExecutionGuard::acquire();
        if !self.acquire_reader() {
            return None;
        }
        Some(BaseRwLockReadGuard {
            lock: self,
            _execution: execution,
        })
    }

    /// Acquires exclusive access while protecting execution state throughout the wait.
    ///
    /// Waits for all readers and writers to leave. A waiting writer does not prevent new readers
    /// from entering, so continuous readers may starve it. Read-to-write upgrading is unsupported.
    #[inline]
    pub fn write(&self) -> BaseRwLockWriteGuard<'_, G, T> {
        let execution = ExecutionGuard::acquire();
        while self
            .state
            .compare_exchange_weak(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.state.load(Ordering::Relaxed) != 0 {
                core::hint::spin_loop();
            }
        }
        BaseRwLockWriteGuard {
            lock: self,
            _execution: execution,
        }
    }

    /// Attempts exclusive access without waiting for readers or another writer.
    ///
    /// Returns None and restores execution state if any guard already holds the lock.
    #[inline]
    pub fn try_write(&self) -> Option<BaseRwLockWriteGuard<'_, G, T>> {
        let execution = ExecutionGuard::acquire();
        if self
            .state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(BaseRwLockWriteGuard {
            lock: self,
            _execution: execution,
        })
    }

    /// Borrows the data mutably without locking when ownership already excludes all guards.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Adds one reader if no writer owns the data, retrying only concurrent reader changes.
    #[inline]
    fn acquire_reader(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & WRITER != 0 {
                return false;
            }
            // Wraparound could make active readers look unlocked and admit a writer. Check
            // before publishing the increment, so panic cannot corrupt the lock state.
            let next = state
                .checked_add(READER)
                .expect("too many reader-writer lock readers");
            match self.state.compare_exchange_weak(
                state,
                next,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => state = current,
            }
        }
    }
}

/// Empty construction for payloads with a default value.
impl<G: BaseGuard, T: Default> Default for BaseRwLock<G, T> {
    /// Creates an unlocked lock containing the payload's default value.
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// Nonblocking formatting for reader-writer locks.
impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseRwLock<G, T> {
    /// Formats shared data or a locked marker when a writer owns it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(data) => f.debug_struct("RwLock").field("data", &&*data).finish(),
            None => f.write_str("RwLock { <locked> }"),
        }
    }
}

/// Shared access through a live reader slot.
impl<G: BaseGuard, T: ?Sized> Deref for BaseRwLockReadGuard<'_, G, T> {
    type Target = T;

    /// Borrows data while this guard prevents every writer from entering.
    fn deref(&self) -> &T {
        // SAFETY: The reader slot remains live until this guard is dropped.
        unsafe { &*self.lock.data.get() }
    }
}

/// Shared borrowing through an exclusive writer.
impl<G: BaseGuard, T: ?Sized> Deref for BaseRwLockWriteGuard<'_, G, T> {
    type Target = T;

    /// Borrows data while this guard excludes all other data access.
    fn deref(&self) -> &T {
        // SAFETY: The writer bit excludes all other readers and writers.
        unsafe { &*self.lock.data.get() }
    }
}

/// Exclusive borrowing through the sole writer guard.
impl<G: BaseGuard, T: ?Sized> DerefMut for BaseRwLockWriteGuard<'_, G, T> {
    /// Borrows data mutably under exclusive ownership.
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The writer bit is held and this guard is borrowed exclusively.
        unsafe { &mut *self.lock.data.get() }
    }
}

/// Read-access release before restoring local execution state.
impl<G: BaseGuard, T: ?Sized> Drop for BaseRwLockReadGuard<'_, G, T> {
    /// Publishes the end of this shared access, then lets the execution guard restore state.
    fn drop(&mut self) {
        self.lock.state.fetch_sub(READER, Ordering::Release);
    }
}

/// Write-access release before restoring local execution state.
impl<G: BaseGuard, T: ?Sized> Drop for BaseRwLockWriteGuard<'_, G, T> {
    /// Publishes all data updates before the execution guard restores state.
    fn drop(&mut self) {
        self.lock.state.store(0, Ordering::Release);
    }
}

/// An IRQ-safe spin lock using the shared per-CPU guard policy.
pub type SpinNoIrq<T> = BaseSpinLock<NoIrqGuardPolicy, T>;

/// A guard for SpinNoIrq.
pub type SpinNoIrqGuard<'a, T> = BaseSpinLockGuard<'a, NoIrqGuardPolicy, T>;

/// A reader-writer lock using the shared per-CPU guard policy.
pub type RwLockNoIrq<T> = BaseRwLock<NoIrqGuardPolicy, T>;

/// A shared guard for RwLockNoIrq.
pub type RwLockNoIrqReadGuard<'a, T> = BaseRwLockReadGuard<'a, NoIrqGuardPolicy, T>;

/// An exclusive guard for RwLockNoIrq.
pub type RwLockNoIrqWriteGuard<'a, T> = BaseRwLockWriteGuard<'a, NoIrqGuardPolicy, T>;
