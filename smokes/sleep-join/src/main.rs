#![no_std]
#![no_main]

use ecraos::{app_main_ap, app_main_bsp, mp::current_cpu_id, reexport::exarch, task};
use log::info;

/// Runs the per-CPU task sleep and join smoke workload.
///
/// Four tasks sleep for one second four times. A fifth task joins all four workers before the
/// application hook returns to the kernel's BSP or AP terminal path.
fn run_sleep_join_smoke() {
    const WORKER_COUNT: usize = 4;
    const SLEEP_ROUNDS: usize = 4;

    let workers: [task::JoinHandle; WORKER_COUNT] = core::array::from_fn(|worker_index| {
        task::spawn(move || {
            for round in 1..=SLEEP_ROUNDS {
                let _ = task::sleep(exarch::time::Duration::from_secs(1));
                info!(
                    "Sleep task {worker_index} on CPU {} completed round {round}/{SLEEP_ROUNDS}",
                    current_cpu_id()
                );
            }
        })
    });

    let mut joiner = task::spawn(move || {
        for (worker_index, mut worker) in workers.into_iter().enumerate() {
            let _ = worker.join();
            info!(
                "Join task on CPU {} joined sleep task {worker_index}/{WORKER_COUNT}",
                current_cpu_id()
            );
        }
        info!(
            "Join task on CPU {} joined all {WORKER_COUNT} sleep tasks",
            current_cpu_id()
        );
    });

    let _ = joiner.join();
}

/// Runs the sleep and join smoke from the BSP application hook.
#[app_main_bsp]
fn main() {
    run_sleep_join_smoke();
}

/// Runs the sleep and join smoke from each AP application hook.
#[app_main_ap]
fn ap_main() {
    run_sleep_join_smoke();
}
