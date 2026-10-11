#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;

use ecraos::{app_main_bsp, reexport::exarch, task};
use log::info;

/// Runs the Stage 2B preemption and synchronization smoke workload.
///
/// CPU-bound workers intentionally avoid yielding so timer preemption is observable. The
/// synchronization workers cover blocking wakeup and semaphore permit reservation.
fn run_sync_preemption_smoke() {
    let domain = task::create_domain(task::DomainPolicy::Preemptive {
        time_slice: exarch::time::Duration::from_millis(10),
    })
    .expect("failed to create preemptive smoke domain");
    task::switch_current_to(&domain).expect("failed to switch into preemptive smoke domain");
    info!("Stage 2B switched into preemptive domain {}", domain.id());

    let cpu_workers: [task::JoinHandle; 2] = core::array::from_fn(|worker| {
        task::spawn(move || {
            let deadline = exarch::time::monotonic_time()
                .saturating_add(exarch::time::Duration::from_millis(100));
            while exarch::time::monotonic_time() < deadline {
                core::hint::spin_loop();
            }
            info!("Preemptive CPU-bound worker {worker} completed");
        })
    });
    for mut worker in cpu_workers {
        worker.join().expect("preemptive worker join failed");
    }
    info!("Stage 2B CPU-bound joins passed");

    let mutex = Arc::new(task::Mutex::new(0usize));
    let condvar = Arc::new(task::Condvar::new());
    let waiter_mutex = Arc::clone(&mutex);
    let waiter_condvar = Arc::clone(&condvar);
    let mut waiter = task::spawn(move || {
        info!("Condvar waiter started");
        let mut guard = waiter_mutex.lock().expect("mutex waiter lock failed");
        while *guard == 0 {
            guard = waiter_condvar
                .wait(guard)
                .expect("condvar waiter block failed");
        }
        info!("Condvar waiter observed notification");
    });
    let notifier_mutex = Arc::clone(&mutex);
    let notifier_condvar = Arc::clone(&condvar);
    let mut notifier = task::spawn(move || {
        info!("Condvar notifier started");
        let mut guard = notifier_mutex.lock().expect("mutex notifier lock failed");
        *guard = 1;
        notifier_condvar.notify_one();
    });
    info!("Stage 2B Condvar tasks spawned");
    waiter.join().expect("condvar waiter join failed");
    notifier.join().expect("condvar notifier join failed");
    info!("Stage 2B Condvar join passed");

    let semaphore = Arc::new(task::Semaphore::new(0, 1));
    let acquire_semaphore = Arc::clone(&semaphore);
    let mut acquirer = task::spawn(move || {
        info!("Semaphore acquirer started");
        acquire_semaphore
            .acquire()
            .expect("semaphore acquire wake failed");
        info!("Semaphore waiter acquired its reserved permit");
    });
    let release_semaphore = Arc::clone(&semaphore);
    let mut releaser = task::spawn(move || {
        info!("Semaphore releaser started");
        release_semaphore
            .release()
            .expect("semaphore release failed");
    });
    info!("Stage 2B Semaphore tasks spawned");
    acquirer.join().expect("semaphore acquirer join failed");
    releaser.join().expect("semaphore releaser join failed");
    info!("Stage 2B preemption and synchronization smoke passed");
}

/// Runs the synchronization and preemption smoke from the BSP application hook.
#[app_main_bsp]
fn main() {
    run_sync_preemption_smoke();
}
