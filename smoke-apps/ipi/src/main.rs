//! Hardware IPI completion and remote scheduling smoke application.
//!
//! Atomic handshakes and monotonic deadlines distinguish completion from elapsed delays.

#![no_std]
#![no_main]

/// The smoke application's allocator-backed snapshot storage.
///
/// Allocation happens only in the BSP test driver, never in an IPI handler.
extern crate alloc;

use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ecraos::{
    app_main_ap, app_main_bsp, foundation_test as test, kprintln, mp,
    reexport::{self, exarch},
    task,
};

/// The preemptive domain published by each application CPU.
///
/// Test topology has four CPUs, and each remote domain contains only its owning CPU.
static DOMAINS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The remote worker's deterministic progress indication.
///
/// Release publication proves that the remote task actually executed its body.
static PROGRESS: AtomicBool = AtomicBool::new(false);

/// Waits for an atomic condition until one absolute monotonic deadline.
///
/// No sleep or yield is used to infer remote progress.
fn wait_until(
    operation: &str,
    target: usize,
    generation: Option<u64>,
    mut complete: impl FnMut() -> bool,
) {
    let deadline = exarch::time::monotonic_time().saturating_add(Duration::from_millis(500));
    while !complete() {
        let observation = exarch::time::monotonic_time();
        assert!(
            observation < deadline,
            "operation={operation} logical={target} physical={:?} generation={generation:?} deadline={deadline:?} observation={observation:?}",
            mp::logical_to_physical(target)
        );
        core::hint::spin_loop();
    }
}

/// Runs each remote root without yielding in a preemptive domain.
///
/// Its five-second time slice exceeds the worker deadline, making prompt IPI scheduling observable.
#[app_main_ap]
fn remote_main() {
    let cpu = mp::current_cpu_id();
    let domain = task::create_domain(task::DomainPolicy::Preemptive {
        time_slice: Duration::from_secs(5),
    })
    .expect("remote domain creation failed");
    task::switch_current_to(&domain).expect("remote domain installation failed");
    DOMAINS[cpu].store(domain.id(), Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

/// Verifies exact targets, direct self delivery, combined reasons and timeout recovery.
///
/// Returning normally follows the unique success marker consumed by the guest harness.
#[app_main_bsp]
fn main() {
    let cpus = test::ready_cpus();
    let current = mp::current_cpu_id();
    assert!(cpus.len() >= 2 && cpus.len() <= DOMAINS.len());
    for &target in cpus.iter().filter(|&&cpu| cpu != current) {
        let before = cpus
            .iter()
            .map(|&cpu| test::counters(cpu).0)
            .collect::<alloc::vec::Vec<_>>();
        let generation =
            test::test_roundtrip(target, Duration::from_millis(500)).expect("remote Test failed");
        for (&cpu, old) in cpus.iter().zip(before) {
            assert_eq!(test::counters(cpu).0, old + u64::from(cpu == target));
        }
        assert_eq!(test::counters(target).2, generation);
        kprintln!(
            "IPI exact-target logical={target} physical={:?} generation={generation}",
            mp::logical_to_physical(target)
        );
    }
    let old = test::counters(current).0;
    let generation =
        test::test_roundtrip(current, Duration::from_millis(500)).expect("self Test failed");
    assert_eq!(test::counters(current).0, old + 1);
    assert_eq!(test::counters(current).2, generation);
    kprintln!("IPI self-target logical={current} generation={generation} direct execution");

    let target = *cpus.iter().find(|&&cpu| cpu != current).unwrap();
    let before = test::counters(target);
    let generation = test::combined_roundtrip(target, Duration::from_millis(500))
        .expect("combined reasons failed");
    wait_until("combined Reschedule", target, Some(generation), || {
        test::counters(target).1 > before.1
    });
    assert_eq!(test::counters(target).0, before.0 + 1);
    kprintln!("IPI combined Test+Reschedule logical={target} generation={generation}");

    wait_until("remote domain ready", target, None, || {
        DOMAINS[target].load(Ordering::Acquire) != 0
    });
    let domain = task::lookup_domain(DOMAINS[target].load(Ordering::Acquire)).unwrap();
    let before = test::counters(target).1;
    let worker = domain
        .spawn(move || {
            assert_eq!(mp::current_cpu_id(), target);
            PROGRESS.store(true, Ordering::Release);
        })
        .expect("remote worker publication failed");
    wait_until("remote preemptive progress", target, None, || {
        PROGRESS.load(Ordering::Acquire)
    });
    assert!(test::counters(target).1 > before);
    kprintln!(
        "IPI remote-progress logical={target} task={} before 500ms with 5s slice",
        worker.id()
    );

    test::suppress_test_ack(target);
    let before = test::counters(target).0;
    let error = test::test_roundtrip(target, Duration::from_millis(20))
        .expect_err("suppressed ack completed");
    let test::IpiError::Timeout(details) = error else {
        panic!("incorrect timeout error: {error:?}")
    };
    assert_eq!(details.logical_cpu, target);
    assert!(details.observed_at >= details.deadline);
    assert_ne!(Some(details.acknowledged), details.generation);
    assert_eq!(test::counters(target).0, before + 1);
    kprintln!("IPI injected-timeout {details:?}");
    test::clear_test_injection(target);
    let generation = test::test_roundtrip(target, Duration::from_millis(500))
        .expect("next generation did not recover");
    assert!(generation > details.generation.unwrap());
    assert_eq!(test::counters(target).2, generation);
    kprintln!("IPI recovered logical={target} exact generation={generation}");
    kprintln!("IPI FOUNDATION SUCCESS");
}
