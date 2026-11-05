//! Behavioral tests for guarded reader-writer locks.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
    vec::Vec,
};

use crate::BaseRwLock;
use kernel_guard::BaseGuard;

/// An unguarded lock for atomic mutual exclusion tests.
type RwLockRaw<T> = BaseRwLock<kernel_guard::NoOp, T>;

thread_local! {
    /// The execution-guard nesting depth on this test thread.
    static GUARD_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// An execution guard that verifies balanced nesting without privileged instructions.
struct TrackingGuard;

impl BaseGuard for TrackingGuard {
    type State = usize;

    fn acquire() -> Self::State {
        GUARD_DEPTH.with(|depth| {
            let previous = depth.get();
            depth.set(previous + 1);
            previous
        })
    }

    fn release(previous: Self::State) {
        GUARD_DEPTH.with(|depth| {
            assert_eq!(depth.get(), previous + 1);
            depth.set(previous);
        });
    }
}

/// Keeps the first shared borrow live until another thread completes its read.
#[test]
fn readers_can_overlap_across_threads() {
    let lock = RwLockRaw::new(7);
    thread::scope(|scope| {
        let first = lock.read();
        let (done, observed) = mpsc::channel();
        let lock_ref = &lock;
        scope.spawn(move || {
            let value = *lock_ref.read();
            let _ = done.send(value);
        });
        let result = observed.recv_timeout(Duration::from_secs(3));
        drop(first);
        assert_eq!(result.unwrap(), 7);
    });
}

/// Excludes writes until the last reader leaves and excludes all access during writes.
#[test]
fn readers_and_writers_preserve_exclusive_mutation() {
    let lock = RwLockRaw::new(5);
    let first = lock.read();
    let second = lock.try_read().unwrap();
    assert!(lock.try_write().is_none());
    drop(first);
    assert!(lock.try_write().is_none());
    drop(second);
    let mut writer = lock.try_write().unwrap();
    assert!(lock.try_read().is_none());
    assert!(lock.try_write().is_none());
    *writer = 9;
    drop(writer);
    assert_eq!(*lock.read(), 9);
}

/// Restores the calling execution state after failed attempts and guard destruction.
#[test]
fn execution_guards_are_balanced_on_success_and_failure() {
    let lock = BaseRwLock::<TrackingGuard, _>::new(0);
    let reader = lock.read();
    assert_eq!(GUARD_DEPTH.with(Cell::get), 1);
    assert!(lock.try_write().is_none());
    assert_eq!(GUARD_DEPTH.with(Cell::get), 1);
    let second = lock.try_read().unwrap();
    assert_eq!(GUARD_DEPTH.with(Cell::get), 2);
    drop(second);
    drop(reader);
    assert_eq!(GUARD_DEPTH.with(Cell::get), 0);
    let writer = lock.write();
    assert!(lock.try_read().is_none());
    assert!(lock.try_write().is_none());
    assert_eq!(GUARD_DEPTH.with(Cell::get), 1);
    drop(writer);
    assert_eq!(GUARD_DEPTH.with(Cell::get), 0);
}

/// Releases the data lock and restores execution protection when a reader or writer unwinds.
#[test]
fn panics_release_both_kinds_of_guard() {
    let lock = BaseRwLock::<TrackingGuard, _>::new(0);
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let _reader = lock.read();
            panic!("reader panic");
        }))
        .is_err()
    );
    assert_eq!(GUARD_DEPTH.with(Cell::get), 0);
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let mut writer = lock.write();
            *writer = 11;
            panic!("writer panic");
        }))
        .is_err()
    );
    assert_eq!(GUARD_DEPTH.with(Cell::get), 0);
    assert_eq!(*lock.try_read().unwrap(), 11);
}

/// Publishes complete updates to concurrent readers without losing writers' increments.
#[test]
fn concurrent_writers_publish_consistent_values() {
    let lock = Arc::new(RwLockRaw::new((0usize, 0usize)));
    thread::scope(|scope| {
        for _ in 0..4 {
            let lock = Arc::clone(&lock);
            scope.spawn(move || {
                for _ in 0..2000 {
                    let mut values = lock.write();
                    values.0 += 1;
                    values.1 += 1;
                }
            });
        }
        for _ in 0..4 {
            let lock = Arc::clone(&lock);
            scope.spawn(move || {
                for _ in 0..2000 {
                    let values = lock.read();
                    assert_eq!(values.0, values.1);
                }
            });
        }
    });
    assert_eq!(*lock.read(), (8000, 8000));
}

/// Supports direct ownership access and the kernel execution-guard aliases.
#[test]
fn ownership_access_and_aliases_work() {
    let mut lock = RwLockRaw::<Vec<usize>>::default();
    lock.get_mut().push(3);
    lock.write().push(4);
    assert_eq!(lock.into_inner(), [3, 4]);
    let sized = RwLockRaw::new([1, 2, 3]);
    let slice_lock: &RwLockRaw<[i32]> = &sized;
    assert_eq!(&*slice_lock.read(), &[1, 2, 3]);
}
