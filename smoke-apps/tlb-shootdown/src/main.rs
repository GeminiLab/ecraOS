//! Cross-CPU translation completion and delayed reuse smoke application.
//!
//! Readers handshake through atomics before and after each mutation without elapsed-delay guesses.

#![no_std]
#![no_main]

/// Allocator-backed test driver storage.
///
/// Allocation never occurs in the interrupt consumer.
extern crate alloc;

use alloc::vec::Vec;
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ecraos::{
    app_main_ap, app_main_bsp, foundation_test as test, kprintln, mp,
    reexport::{self, exarch},
    task,
};

/// The one-CPU domains used to place remote readers.
///
/// All logical membership comes from the production MP snapshot.
static DOMAINS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The reader command generation.
///
/// Each Release publication follows completed shootdown and precedes the reader's Acquire load.
static COMMAND: AtomicUsize = AtomicUsize::new(0);

/// Each reader's completed observation generation.
///
/// Only its owning logical CPU publishes this barrier acknowledgement.
static OBSERVED: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];

/// Waits for a concrete condition under one absolute monotonic deadline.
///
/// A missing observation fails explicitly and reports every incomplete logical/physical target.
fn wait_until(
    operation: &str,
    targets: &[usize],
    generation: Option<u64>,
    mut complete: impl FnMut(usize) -> bool,
) {
    let deadline = exarch::time::monotonic_time().saturating_add(Duration::from_secs(2));
    while !targets.iter().all(|&cpu| complete(cpu)) {
        let observation = exarch::time::monotonic_time();
        if observation >= deadline {
            let missing = targets
                .iter()
                .copied()
                .filter(|&cpu| !complete(cpu))
                .map(|cpu| {
                    (
                        cpu,
                        mp::logical_to_physical(cpu),
                        OBSERVED[cpu].load(Ordering::Acquire),
                    )
                })
                .collect::<Vec<_>>();
            panic!(
                "operation={operation} missing=(logical,physical,observed-command){missing:?} generation={generation:?} command={} deadline={deadline:?} observation={observation:?}",
                COMMAND.load(Ordering::Acquire)
            );
        }
        core::hint::spin_loop();
    }
}

/// Runs a CPU-bound root until remote reader publication preempts it.
///
/// The five-second slice exceeds the reader-start deadline and requires remote scheduling progress.
#[app_main_ap]
fn remote_main() {
    let cpu = mp::current_cpu_id();
    let domain = task::create_domain(task::DomainPolicy::Preemptive {
        time_slice: Duration::from_secs(5),
    })
    .unwrap();
    task::switch_current_to(&domain).unwrap();
    DOMAINS[cpu].store(domain.id(), Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

/// Publishes one observation command and awaits every explicit reader barrier.
///
/// Readers are parked by command five before any shared range is unmapped permanently.
fn command(targets: &[usize], generation: usize, tlb_generation: Option<u64>) {
    COMMAND.store(generation, Ordering::Release);
    wait_until(
        "remote observation barrier",
        targets,
        tlb_generation,
        |cpu| OBSERVED[cpu].load(Ordering::Acquire) == generation,
    );
}

/// Proves invalidation variants, exact completion, reuse and timeout quarantine.
///
/// The unique success marker appears only after all deterministic assertions have completed.
#[app_main_bsp]
fn main() {
    let cpus = test::ready_cpus();
    assert_eq!(cpus.len(), 4);
    let targets = cpus
        .into_iter()
        .filter(|&cpu| cpu != mp::current_cpu_id())
        .collect::<Vec<_>>();
    let range = test::allocate_shared(&[0x11, 0x22]).unwrap();
    let size = test::page_size();
    let mut readers = Vec::new();
    for &cpu in &targets {
        wait_until("remote domain ready", &[cpu], None, |cpu| {
            DOMAINS[cpu].load(Ordering::Acquire) != 0
        });
        let domain = task::lookup_domain(DOMAINS[cpu].load(Ordering::Acquire)).unwrap();
        let reader = domain
            .spawn(move || {
                assert_eq!(mp::current_cpu_id(), cpu);
                assert!(
                    exarch::trap::local_enabled(),
                    "remote reader entered with IRQs disabled"
                );
                let mut seen = 0;
                let deadline =
                    exarch::time::monotonic_time().saturating_add(Duration::from_secs(20));
                loop {
                    let observation = exarch::time::monotonic_time();
                    let stage = COMMAND.load(Ordering::Acquire);
                    assert!(
                        observation < deadline,
                        "operation=reader-command logical={cpu} physical={:?} generation={stage} seen={seen} deadline={deadline:?} observation={observation:?}",
                        mp::logical_to_physical(cpu)
                    );
                    if stage == seen || stage == 0 {
                        core::hint::spin_loop();
                        continue;
                    }
                    if stage == 6 {
                        break;
                    }
                    if stage < 5 {
                        let expected = match stage {
                            1 => [0x11, 0x22],
                            2 => [0x33, 0x22],
                            3 => [0x44, 0x55],
                            4 => [0x66, 0x77],
                            _ => unreachable!(),
                        };
                        for (index, value) in expected.into_iter().enumerate() {
                            // SAFETY: The barrier keeps this range mapped until every reader parks.
                            let observed = unsafe {
                                (range.start + index * size)
                                    .as_ptr_of::<u64>()
                                    .read_volatile()
                            };
                            assert_eq!(
                                observed, value,
                                "stale remote translation cpu={cpu} stage={stage} page={index}"
                            );
                        }
                    }
                    seen = stage;
                    OBSERVED[cpu].store(stage, Ordering::Release);
                }
            })
            .unwrap();
        readers.push(reader);
    }
    command(&targets, 1, None);
    let page = test::page_range(range.start);
    let generation =
        test::replace_shared(page, &[0x33], test::Invalidation::Page(range.start)).unwrap();
    command(&targets, 2, Some(generation));
    test::assert_tlb_completed(&targets, generation);
    kprintln!(
        "TLB Page exact generation={generation} targets={targets:?} remote-observation=[0x33,0x22] adjacent=unchanged"
    );

    let generation =
        test::replace_shared(range, &[0x44, 0x55], test::Invalidation::Range(range)).unwrap();
    command(&targets, 3, Some(generation));
    test::assert_tlb_completed(&targets, generation);
    kprintln!(
        "TLB Range exact generation={generation} targets={targets:?} remote-observation=[0x44,0x55]"
    );

    let generation = test::replace_shared(range, &[0x66, 0x77], test::Invalidation::All).unwrap();
    command(&targets, 4, Some(generation));
    test::assert_tlb_completed(&targets, generation);
    kprintln!(
        "TLB All exact generation={generation} targets={targets:?} remote-observation=[0x66,0x77]"
    );
    command(&targets, 5, Some(generation));

    let frames = test::backing_frames(range);
    test::deallocate_shared(range.start).unwrap();
    assert!(!test::reservation_retained(range.start));
    for &frame in &frames {
        assert!(
            test::claim_released_frame(frame),
            "successful frame not reusable: {frame:#x}"
        );
    }
    let reused = test::allocate_shared(&[0x88, 0x99]).unwrap();
    assert_eq!(reused, range);
    kprintln!(
        "TLB reuse completed frames={frames:?} VA={:#x} guarded-reservation=reused",
        reused
    );

    let frames = test::backing_frames(reused);
    let target = targets[0];
    test::suppress_tlb_ack(target);
    let error = test::deallocate_shared(reused.start).expect_err("suppressed completion succeeded");
    let test::IpiError::Timeout(details) =
        test::completion_ipi_error(&error).expect("missing completion diagnostic")
    else {
        panic!("wrong quarantine error: {error:?}")
    };
    assert_eq!(details.logical_cpu, target);
    assert!(details.observed_at >= details.deadline);
    assert!(test::reservation_quarantined(reused.start));
    assert!(test::reservation_retained(reused.start - size));
    assert!(test::reservation_retained(reused.end));
    assert!(test::backing_not_live(reused.start));
    for &frame in &frames {
        assert!(test::frame_allocated(frame));
        assert!(
            !test::claim_released_frame(frame),
            "quarantined frame reused"
        );
    }
    assert!(test::remap_quarantined(reused).is_err());
    let distinct = test::allocate_shared(&[0xaa, 0xbb]).unwrap();
    assert_ne!(distinct, reused);
    kprintln!(
        "TLB quarantine {details:?} frames={frames:?} VA={:#x} PTE=cleared live=false frame/VA/guards=retained retry=rejected nextVA={:#x}",
        reused,
        distinct
    );
    test::clear_tlb_injection(target);
    let generation = test::flush_all().unwrap();
    assert!(generation > details.generation.unwrap());
    test::assert_tlb_completed(&targets, generation);
    kprintln!("TLB recovery exact generation={generation} targets={targets:?}");

    COMMAND.store(6, Ordering::Release);
    for reader in &readers {
        reader.join().unwrap();
    }
    wait_until("reader stack handoff", &targets, Some(generation), |cpu| {
        let index = targets.iter().position(|&target| target == cpu).unwrap();
        !test::task_stack_current(&readers[index])
    });
    let reader_stacks = readers
        .iter()
        .map(test::task_stack_start)
        .collect::<Vec<_>>();
    drop(readers);
    wait_until("reader stack release", &targets, Some(generation), |cpu| {
        test::reap_stacks();
        let index = targets.iter().position(|&target| target == cpu).unwrap();
        !test::reservation_retained(reader_stacks[index])
    });
    let mut previous = None;
    for iteration in 0..4 {
        let mut handle = task::spawn(move || {
            let stack = test::current_stack_start();
            let local = [0x5au64; 32];
            assert_eq!(local[iteration], 0x5a);
            STACK_START.store(stack.as_usize(), Ordering::Release);
        });
        handle.join().unwrap();
        test::reap_stacks();
        let start = STACK_START.load(Ordering::Acquire);
        if let Some(old) = previous {
            assert_eq!(start, old, "completed stack reservation not reused");
        }
        previous = Some(start);
        kprintln!(
            "TLB stack iteration={iteration} VA={start:#x} exited/reaped/acknowledged reuse=safe"
        );
    }
    kprintln!("TLB SHOOTDOWN FOUNDATION SUCCESS");
}

/// The exited local task's observed stack address.
///
/// Release publication is read only after its completion and safe-context reclamation.
static STACK_START: AtomicUsize = AtomicUsize::new(0);
