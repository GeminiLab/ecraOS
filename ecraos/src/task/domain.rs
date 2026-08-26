//! Scheduling-domain metadata and CPU membership.
//!
//! A domain owns policy and accounting while physical queues remain owned by CPUs. The bounded
//! bitmap is intentionally local to the task layer for Stage 2B and may later become a lock-free
//! membership structure after a memory-ordering proof.

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use kspin::SpinNoIrq;
use lazyinit::LazyInit;

use super::{JoinHandle, Task};

/// The temporary maximum logical CPU count supported by Stage 2B.
pub const MAX_CPU_NUM: usize = 256;

/// A monotonic scheduling-domain identifier.
pub type DomainId = u64;

/// An immutable domain scheduling policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainPolicy {
    /// Runs tasks only at explicit scheduler safe points.
    Cooperative,
    /// Runs tasks with a positive timer-driven slice.
    Preemptive {
        /// The requested wall-clock time slice.
        time_slice: Duration,
    },
}

impl DomainPolicy {
    /// Returns the policy's optional time slice.
    pub const fn time_slice(self) -> Option<Duration> {
        match self {
            Self::Cooperative => None,
            Self::Preemptive { time_slice } => Some(time_slice),
        }
    }

    /// Converts a preemptive time slice to timer ticks by upward rounding.
    pub fn slice_ticks(self, tick: Duration) -> Option<u64> {
        let time_slice = self.time_slice()?;
        assert!(!tick.is_zero(), "timer tick must be positive");
        let slice_nanos = u64::try_from(time_slice.as_nanos()).unwrap_or(u64::MAX);
        let tick_nanos = u64::try_from(tick.as_nanos()).unwrap_or(u64::MAX);
        Some(slice_nanos.div_ceil(tick_nanos).max(1))
    }
}

/// The explicit lifecycle of one scheduling domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainLifecycle {
    /// Accepts new tasks and CPU membership changes.
    Active,
    /// Rejects new work while existing work drains.
    Draining,
    /// Awaiting final ownership removal.
    Destroying,
    /// Permanently unusable.
    Destroyed,
}

/// Errors returned by domain operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainError {
    /// The monotonic identifier space is exhausted.
    IdentifierExhausted,
    /// The policy contains a zero time slice.
    InvalidPolicy,
    /// The target CPU is outside the task-layer bound.
    CpuOutOfRange,
    /// The target task or CPU ownership is invalid.
    InvalidTarget,
    /// The operation is incompatible with the current lifecycle.
    DomainDestroying,
    /// The domain has reached its terminal lifecycle.
    DomainDestroyed,
    /// Outstanding ownership prevents destruction.
    Busy,
}

/// A fixed 256-bit logical CPU membership set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuSet([u64; MAX_CPU_NUM / 64]);

impl CpuSet {
    /// Creates an empty CPU set.
    pub const fn empty() -> Self {
        Self([0; MAX_CPU_NUM / 64])
    }

    /// Adds one logical CPU and rejects values outside the Stage 2B bound.
    pub fn insert(&mut self, cpu: usize) -> Result<(), DomainError> {
        let (word, bit) = Self::position(cpu)?;
        self.0[word] |= 1u64 << bit;
        Ok(())
    }

    /// Removes one logical CPU and rejects values outside the Stage 2B bound.
    pub fn remove(&mut self, cpu: usize) -> Result<(), DomainError> {
        let (word, bit) = Self::position(cpu)?;
        self.0[word] &= !(1u64 << bit);
        Ok(())
    }

    /// Returns whether one logical CPU is a member.
    pub fn contains(&self, cpu: usize) -> bool {
        Self::position(cpu)
            .map(|(word, bit)| self.0[word] & (1u64 << bit) != 0)
            .unwrap_or(false)
    }

    /// Returns a sorted bounded snapshot of all members.
    pub fn snapshot(&self) -> Vec<usize> {
        (0..MAX_CPU_NUM).filter(|&cpu| self.contains(cpu)).collect()
    }

    fn position(cpu: usize) -> Result<(usize, usize), DomainError> {
        if cpu >= MAX_CPU_NUM {
            return Err(DomainError::CpuOutOfRange);
        }
        Ok((cpu / 64, cpu % 64))
    }
}

/// A reference-counted scheduling domain.
pub type DomainRef = Arc<SchedulingDomain>;

/// Weak domain registry used for diagnostics and identity lookup.
static DOMAIN_REGISTRY: LazyInit<SpinNoIrq<Vec<Weak<SchedulingDomain>>>> = LazyInit::new();

/// Domain metadata, policy, ownership, and suspended runnable tasks.
pub struct SchedulingDomain {
    id: DomainId,
    policy: DomainPolicy,
    lifecycle: SpinNoIrq<DomainLifecycle>,
    cpus: SpinNoIrq<CpuSet>,
    suspended: SpinNoIrq<VecDeque<Arc<Task>>>,
    task_count: AtomicUsize,
    in_flight: AtomicUsize,
    placement_cursor: AtomicUsize,
}

static NEXT_DOMAIN_ID: AtomicU64 = AtomicU64::new(1);

impl SchedulingDomain {
    /// Returns the stable domain identifier.
    pub const fn id(&self) -> DomainId {
        self.id
    }

    /// Returns the immutable scheduling policy.
    pub const fn policy(&self) -> DomainPolicy {
        self.policy
    }

    /// Returns the current lifecycle state.
    pub fn lifecycle(&self) -> DomainLifecycle {
        // Lock order is NoPreemptIrqSave, then domain metadata locks in ascending DomainId.
        // A future lock-free lifecycle structure may replace this lock after its
        // memory-ordering proof.
        let _guard = kernel_guard::NoPreempt::new();
        *self.lifecycle.lock()
    }

    /// Returns a bounded CPU-membership snapshot in ascending order.
    pub fn cpu_snapshot(&self) -> CpuSet {
        // Lock order is NoPreemptIrqSave, then domain metadata locks in ascending DomainId.
        // A future lock-free CPU-membership structure may replace this lock after its
        // memory-ordering proof.
        let _guard = kernel_guard::NoPreempt::new();
        *self.cpus.lock()
    }

    /// Returns the number of non-exited tasks accounted to this domain.
    pub fn task_count(&self) -> usize {
        self.task_count.load(Ordering::Acquire)
    }

    /// Creates and publishes one task in this domain.
    pub fn spawn(
        self: &DomainRef,
        body: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Task>, DomainError> {
        // Keep the in-flight operation visible while the lifecycle is checked. Destruction takes
        // this same metadata lock before checking the counter, so it cannot pass this operation.
        // Lock order is NoPreemptIrqSave, then domain metadata locks in ascending DomainId.
        // A future lock-free lifecycle/accounting structure may replace these locks after its
        // memory-ordering proof.
        let _guard = kernel_guard::NoPreempt::new();
        let lifecycle = self.lifecycle.lock();
        match *lifecycle {
            DomainLifecycle::Active => {}
            DomainLifecycle::Destroyed => return Err(DomainError::DomainDestroyed),
            DomainLifecycle::Draining | DomainLifecycle::Destroying => {
                return Err(DomainError::DomainDestroying);
            }
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        drop(lifecycle);
        let result = super::scheduler::spawn_in_domain(self, body);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        result
    }

    /// Creates one task and returns both its strong reference and join handle.
    pub fn spawn_joinable(
        self: &DomainRef,
        body: impl FnOnce() + Send + 'static,
    ) -> Result<(Arc<Task>, JoinHandle), DomainError> {
        let task = self.spawn(body)?;
        let handle = task.join_handle();
        Ok((task, handle))
    }

    /// Adds one CPU to this domain.
    pub fn add_cpu(&self, cpu: usize) -> Result<(), DomainError> {
        // Lock order is NoPreemptIrqSave, then this domain metadata lock, then any CPU Queue lock.
        // A future lock-free CPU-membership structure may replace this lock after its
        // memory-ordering proof.
        let _guard = kernel_guard::NoPreempt::new();
        let lifecycle = self.lifecycle.lock();
        if *lifecycle != DomainLifecycle::Active {
            return Err(DomainError::DomainDestroying);
        }
        let result = self.cpus.lock().insert(cpu);
        drop(lifecycle);
        if result.is_ok() && super::scheduler::initialized() {
            super::scheduler::drain_suspended(self, cpu);
        }
        result
    }

    /// Removes one CPU from this domain when it has no queued ownership.
    pub fn remove_cpu(&self, cpu: usize) -> Result<(), DomainError> {
        // Lock order is NoPreemptIrqSave, then this domain metadata lock, then any CPU Queue lock.
        // A future lock-free CPU-membership structure may replace this lock after its
        // memory-ordering proof.
        let lifecycle = self.lifecycle.lock();
        if *lifecycle != DomainLifecycle::Active {
            return Err(DomainError::DomainDestroying);
        }
        self.cpus.lock().remove(cpu)
    }

    /// Selects the next active CPU using a round-robin cursor.
    pub fn select_cpu(&self) -> Option<usize> {
        let cpus = self.cpu_snapshot().snapshot();
        if cpus.is_empty() {
            return None;
        }
        let cursor = self.placement_cursor.fetch_add(1, Ordering::Relaxed);
        cpus.get(cursor % cpus.len()).copied()
    }

    /// Begins explicit domain destruction after all ownership has drained.
    pub fn destroy(&self) -> Result<(), DomainError> {
        // Lifecycle, task accounting, suspended ownership, and CPU membership are checked under
        // the domain lock order. No queue or task lock is acquired here. A future lock-free
        // destruction protocol may replace these locks after its memory-ordering proof.
        let _guard = kernel_guard::NoPreempt::new();
        let mut lifecycle = self.lifecycle.lock();
        match *lifecycle {
            DomainLifecycle::Active => *lifecycle = DomainLifecycle::Draining,
            DomainLifecycle::Draining => {}
            DomainLifecycle::Destroying | DomainLifecycle::Destroyed => {
                return Err(DomainError::DomainDestroyed);
            }
        }
        if self.task_count() != 0
            || self.in_flight.load(Ordering::Acquire) != 0
            || !self.suspended.lock().is_empty()
            || !self.cpu_snapshot().snapshot().is_empty()
        {
            return Err(DomainError::Busy);
        }
        *lifecycle = DomainLifecycle::Destroying;
        *lifecycle = DomainLifecycle::Destroyed;
        Ok(())
    }

    pub(super) fn account_task(&self) {
        self.task_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Publishes that one accounted task reached the Exited state.
    pub(super) fn retire_task(&self) {
        let previous = self.task_count.fetch_sub(1, Ordering::AcqRel);
        assert_ne!(previous, 0, "domain task accounting underflow");
    }
    pub(super) fn suspend(&self, task: Arc<Task>) {
        let _guard = kernel_guard::NoPreempt::new();
        self.suspended.lock().push_back(task);
    }

    /// Removes suspended runnable tasks for publication after CPU membership returns.
    pub(super) fn take_suspended(&self) -> VecDeque<Arc<Task>> {
        let _guard = kernel_guard::NoPreempt::new();
        core::mem::take(&mut *self.suspended.lock())
    }

    /// Returns the number of suspended runnable tasks.
    pub fn suspended_len(&self) -> usize {
        let _guard = kernel_guard::NoPreempt::new();
        self.suspended.lock().len()
    }
}

/// Creates an empty scheduling domain with immutable policy.
pub fn create_domain(policy: DomainPolicy) -> Result<DomainRef, DomainError> {
    if let DomainPolicy::Preemptive { time_slice } = policy {
        if time_slice.is_zero() {
            return Err(DomainError::InvalidPolicy);
        }
    }
    let id = NEXT_DOMAIN_ID
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
        .map_err(|_| DomainError::IdentifierExhausted)?;
    let domain = Arc::new(SchedulingDomain {
        id,
        policy,
        lifecycle: SpinNoIrq::new(DomainLifecycle::Active),
        cpus: SpinNoIrq::new(CpuSet::empty()),
        suspended: SpinNoIrq::new(VecDeque::new()),
        task_count: AtomicUsize::new(0),
        in_flight: AtomicUsize::new(0),
        placement_cursor: AtomicUsize::new(0),
    });
    if DOMAIN_REGISTRY.get().is_none() {
        DOMAIN_REGISTRY.init_once(SpinNoIrq::new(Vec::new()));
    }
    DOMAIN_REGISTRY
        .get()
        .expect("domain registry initialized")
        .lock()
        .push(Arc::downgrade(&domain));
    Ok(domain)
}

/// Looks up a domain without extending its lifetime through the registry.
pub fn lookup_domain(id: DomainId) -> Option<DomainRef> {
    let registry = DOMAIN_REGISTRY.get()?;
    let mut entries = registry.lock();
    let mut found = None;
    entries.retain(|weak| {
        let Some(domain) = weak.upgrade() else {
            return false;
        };
        if domain.id() == id {
            found = Some(domain);
        }
        true
    });
    found
}

/// Returns a weak domain reference for task ownership fields.
pub(super) fn downgrade(domain: &DomainRef) -> Weak<SchedulingDomain> {
    Arc::downgrade(domain)
}

#[cfg(test)]
mod tests {
    use super::{CpuSet, DomainError, DomainLifecycle, DomainPolicy, MAX_CPU_NUM, create_domain};

    #[test]
    fn cpu_set_rejects_the_stage_2b_upper_bound() {
        let mut cpus = CpuSet::empty();
        assert_eq!(cpus.insert(MAX_CPU_NUM), Err(DomainError::CpuOutOfRange));
        cpus.insert(MAX_CPU_NUM - 1).unwrap();
        assert_eq!(cpus.snapshot(), alloc::vec![MAX_CPU_NUM - 1]);
    }

    #[test]
    fn busy_destroy_remains_draining() {
        let domain = create_domain(DomainPolicy::Cooperative).unwrap();
        domain.add_cpu(0).unwrap();
        assert_eq!(domain.destroy(), Err(DomainError::Busy));
        assert_eq!(domain.lifecycle(), DomainLifecycle::Draining);
    }

    #[test]
    fn preemptive_slice_rounds_up_to_one_tick() {
        let policy = DomainPolicy::Preemptive {
            time_slice: core::time::Duration::from_nanos(11),
        };
        assert_eq!(
            policy.slice_ticks(core::time::Duration::from_nanos(10)),
            Some(2)
        );
    }
}
