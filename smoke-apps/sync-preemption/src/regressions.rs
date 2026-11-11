//! Deterministic lost-wakeup and independent semaphore handoff regressions.
//!
//! These checks run in the BSP root task's cooperative domain. Explicit scheduling gaps force
//! the failing interleavings without depending on timer delivery or elapsed delays.

use alloc::sync::Arc;
use core::time::Duration;

use ecraos::{foundation_test as test, task};
use log::info;

/// Runs each regression before the BSP enters the preemptive smoke domain.
pub(super) fn check() {
    semaphore_release_before_registration();
    mutex_unlock_before_registration();
    semaphore_releases_preserve_each_waiter();
    stale_timer_cannot_grant_a_permit();
    info!("Synchronization registration and handoff regressions passed");
}

/// Verifies a release between the initial zero-permit observation and registration.
fn semaphore_release_before_registration() {
    let semaphore = Arc::new(task::Semaphore::new(0, 1));
    let acquire_semaphore = Arc::clone(&semaphore);
    let mut acquirer = task::spawn(move || {
        test::yield_before_next_sync_wait();
        acquire_semaphore.acquire().expect("gap acquisition failed");
    });
    let release_semaphore = Arc::clone(&semaphore);
    let mut releaser = task::spawn(move || {
        release_semaphore.release().expect("gap release failed");
    });
    releaser.join().expect("gap releaser join failed");
    task::yield_now();
    assert!(
        acquirer.is_finished(),
        "semaphore lost a release before waiter registration"
    );
    acquirer.join().expect("gap acquirer join failed");
    assert_eq!(semaphore.available(), 0);
    info!("Semaphore release-before-registration regression passed");
}

/// Verifies an unlock between the failed mutex acquisition and waiter registration.
fn mutex_unlock_before_registration() {
    let mutex = Arc::new(task::Mutex::new(0usize));
    let guard = mutex.lock().expect("gap owner lock failed");
    let waiter_mutex = Arc::clone(&mutex);
    let mut waiter = task::spawn(move || {
        test::yield_before_next_sync_wait();
        let mut guard = waiter_mutex.lock().expect("gap waiter lock failed");
        *guard += 1;
    });
    task::yield_now();
    drop(guard);
    task::yield_now();
    assert!(
        waiter.is_finished(),
        "mutex lost an unlock before waiter registration"
    );
    waiter.join().expect("gap mutex waiter join failed");
    assert_eq!(*mutex.lock().expect("gap result lock failed"), 1);
    assert_eq!(mutex.waiter_count(), 0);
    info!("Mutex unlock-before-registration regression passed");
}

/// Verifies two releases grant distinct blocked waiters before either one resumes.
fn semaphore_releases_preserve_each_waiter() {
    let semaphore = Arc::new(task::Semaphore::new(0, 2));
    let mut workers: [task::JoinHandle; 2] = core::array::from_fn(|_| {
        let semaphore = Arc::clone(&semaphore);
        task::spawn(move || {
            semaphore.acquire().expect("handoff acquisition failed");
        })
    });
    task::yield_now();
    assert!(workers.iter().all(|worker| !worker.is_finished()));
    semaphore.release().expect("first handoff release failed");
    semaphore.release().expect("second handoff release failed");
    assert_eq!(semaphore.available(), 0);
    task::yield_now();
    assert!(
        workers.iter().all(task::JoinHandle::is_finished),
        "semaphore overwrote an earlier waiter's permit"
    );
    for worker in &mut workers {
        worker.join().expect("handoff worker join failed");
    }
    assert_eq!(semaphore.available(), 0);
    info!("Semaphore independent-handoff regression passed");
}

/// Verifies a stale timer wake cannot complete a semaphore acquisition without a grant.
fn stale_timer_cannot_grant_a_permit() {
    let semaphore = Arc::new(task::Semaphore::new(0, 1));
    let acquire_semaphore = Arc::clone(&semaphore);
    let domain = task::current_task()
        .expect("missing root task")
        .domain()
        .expect("missing root domain");
    let (waiter, mut acquirer) = domain
        .spawn_joinable(move || {
            task::sleep(Duration::from_millis(1)).expect("timer setup sleep failed");
            acquire_semaphore
                .acquire()
                .expect("post-timer acquire failed");
        })
        .expect("post-timer worker spawn failed");
    let deadline =
        ecraos::reexport::exarch::time::monotonic_time().saturating_add(Duration::from_millis(500));
    while waiter.state() != task::TaskState::Blocked {
        assert!(
            ecraos::reexport::exarch::time::monotonic_time() < deadline,
            "post-timer worker did not block"
        );
        task::yield_now();
    }
    test::wake_from_stale_sleep_timer(waiter.clone());
    task::yield_now();
    assert!(
        !acquirer.is_finished(),
        "semaphore returned without a permit after a stale timer wake"
    );
    assert_eq!(waiter.state(), task::TaskState::Blocked);
    assert_eq!(semaphore.available(), 0);
    semaphore.release().expect("post-timer release failed");
    acquirer.join().expect("post-timer acquirer join failed");
    assert_eq!(semaphore.available(), 0);
    info!("Semaphore stale-timer-wake regression passed");
}
