//! Hardware IPI completion smoke application.
//!
//! Atomic handshakes and monotonic deadlines distinguish completion from elapsed delays.

#![no_std]
#![no_main]

/// The smoke application's allocator-backed snapshot storage.
///
/// Allocation happens only in the BSP test driver, never in an IPI handler.
extern crate alloc;

use core::time::Duration;

use ecraos::{
    app_main_ap, app_main_bsp, foundation_test as test, kprintln, mp,
    reexport::{self, exarch},
};

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

/// Keeps each remote CPU available for hardware IPI delivery.
///
/// The root spins with local interrupts enabled by the kernel timer initialization.
#[app_main_ap]
fn remote_main() {
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
    assert!(cpus.len() >= 2);
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
