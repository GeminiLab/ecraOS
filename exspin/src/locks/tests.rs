//! Overflow and execution-guard ordering tests for reader-writer locks.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::{BaseGuard, BaseRwLock, Ordering};

thread_local! {
    /// The execution-state depth used by the overflow probe.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Whether the next restoration must verify that the data lock is unlocked.
    static CHECK_RESTORE: Cell<bool> = const { Cell::new(false) };
}

/// Execution-state tracking used when a synthetic reader count overflows.
struct OverflowGuard;

impl BaseGuard for OverflowGuard {
    type State = usize;

    fn acquire() -> Self::State {
        DEPTH.with(|depth| depth.replace(depth.get() + 1))
    }

    fn release(previous: Self::State) {
        DEPTH.with(|depth| depth.set(previous));
    }
}

/// Refuses overflow without publishing a wrapped count or leaking execution protection.
#[test]
fn reader_overflow_preserves_state_and_restores_execution_guard() {
    let lock = BaseRwLock::<OverflowGuard, _>::new(());
    lock.state.store(usize::MAX - 1, Ordering::Relaxed);
    for blocking in [false, true] {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                if blocking {
                    drop(lock.read());
                } else {
                    drop(lock.try_read());
                }
            }))
            .is_err()
        );
        assert_eq!(lock.state.load(Ordering::Relaxed), usize::MAX - 1);
        assert_eq!(DEPTH.with(Cell::get), 0);
    }
}

/// Execution restoration that probes the original lock for exclusive access.
struct RestoreGuard;

/// The lock reentered by RestoreGuard after its original reader or writer is released.
static RESTORE_LOCK: BaseRwLock<RestoreGuard, ()> = BaseRwLock::new(());

impl BaseGuard for RestoreGuard {
    type State = ();

    fn acquire() -> Self::State {}

    fn release(_: Self::State) {
        if CHECK_RESTORE.with(|check| check.replace(false)) {
            assert!(
                RESTORE_LOCK.try_write().is_some(),
                "restore ran before data unlock"
            );
        }
    }
}

/// Allows execution-restoration callbacks to reenter after either lock mode is released.
#[test]
fn data_unlock_precedes_execution_state_restoration() {
    let reader = RESTORE_LOCK.read();
    CHECK_RESTORE.with(|check| check.set(true));
    drop(reader);
    let writer = RESTORE_LOCK.write();
    CHECK_RESTORE.with(|check| check.set(true));
    drop(writer);
}

/// Execution-guard count for the sole test that blocks a writer behind a reader.
static WAIT_GUARDS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Execution policy that exposes protection held by a blocked acquisition.
struct WaitingGuard;

impl BaseGuard for WaitingGuard {
    type State = ();

    fn acquire() -> Self::State {
        WAIT_GUARDS.fetch_add(1, Ordering::SeqCst);
    }

    fn release(_: Self::State) {
        WAIT_GUARDS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Protects execution while a writer waits, rather than only after it obtains data access.
#[test]
fn blocked_writer_keeps_execution_protected() {
    let lock = BaseRwLock::<WaitingGuard, _>::new(());
    thread::scope(|scope| {
        let reader = lock.read();
        let (entered, received) = mpsc::channel();
        let lock_ref = &lock;
        scope.spawn(move || {
            let _writer = lock_ref.write();
            let _ = entered.send(());
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while WAIT_GUARDS.load(Ordering::SeqCst) != 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        let protected = WAIT_GUARDS.load(Ordering::SeqCst) == 2;
        let blocked = matches!(received.try_recv(), Err(mpsc::TryRecvError::Empty));
        drop(reader);
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(protected, "waiting must already hold execution protection");
        assert!(blocked, "the writer must still wait for the reader");
    });
    assert_eq!(WAIT_GUARDS.load(Ordering::SeqCst), 0);
}
