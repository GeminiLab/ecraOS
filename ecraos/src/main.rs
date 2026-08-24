//! `ecraOS` kernel binary.
//!
//! This binary is the core and PIE-enabled part of the kernel. For the static
//! loader stage (handling very early boot and initialization), see [`ecraldr_base`]
//! and `ecraldr`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![deny(unfulfilled_lint_expectations)]

/// The Rust standard allocator interface.
///
/// Required for `alloc` crate types (`Box`, `Vec`, `String`, etc.) to be
/// available in the kernel.
extern crate alloc;

#[cfg(target_arch = "x86_64")]
use core::sync::atomic::{AtomicUsize, Ordering};

use log::{error, info};

mod device;
mod irq;
mod kernel_if;
mod logging;
mod mem;
mod mp;
mod percpu;
mod smoke;
mod task;
mod timer;

macro_rules! kprintln {
    ($($arg:tt)*) => {
        exarch::dbcn_println!($($arg)*)
    };
}

pub(crate) use kprintln;

/// Banner line printed at startup.
const HELLO_ECRAOS: &str = "Hello, ecraOS!";
/// Attribution line printed at startup.
const DISCLAIMER: &str = "ecraOS is a derivative of the ArceOS project.";
/// Horizontal line printed at startup.
const HLINE: &str = "------------------------------------------------------------";

/// Runs the per-CPU task sleep and join smoke workload.
///
/// Four tasks each sleep for one second four times. A fifth task joins all four workers before
/// the per-CPU root task performs the BSP or AP terminal transition.
fn run_sleep_join_workload(is_bsp: bool) -> ! {
    const WORKER_COUNT: usize = 4;
    const SLEEP_ROUNDS: usize = 4;

    let workers: [task::JoinHandle; WORKER_COUNT] = core::array::from_fn(|worker_index| {
        task::spawn(move || {
            for round in 1..=SLEEP_ROUNDS {
                task::sleep(exarch::time::Duration::from_secs(1));
                info!(
                    "Sleep task {worker_index} on CPU {} completed round {round}/{SLEEP_ROUNDS}",
                    mp::current_cpu_id()
                );
            }
        })
    });

    let joiner = task::spawn(move || {
        for (worker_index, worker) in workers.into_iter().enumerate() {
            worker.join();
            info!(
                "Join task on CPU {} joined sleep task {worker_index}/{WORKER_COUNT}",
                mp::current_cpu_id()
            );
        }
        info!(
            "Join task on CPU {} joined all {WORKER_COUNT} sleep tasks",
            mp::current_cpu_id()
        );
    });

    joiner.join();
    if is_bsp {
        exarch::power::poweroff()
    } else {
        task::run_idle()
    }
}

fn print_hello_banner() {
    kprintln!("\n\n{HLINE}\n{HELLO_ECRAOS}\n\n{DISCLAIMER}\n{HLINE}");
    kprintln!(
        "Target triple: {}\nHost triple  : {}\n{HLINE}\n",
        target_triple::TARGET,
        target_triple::HOST
    );
}

/// Kernel entry called by specified boot modules through
/// [`call_kernel_entry`](ecraldr_base::call_kernel_entry).
///
/// For execution environment requirements when calling this function, see
/// [`call_kernel_entry`](ecraldr_base::call_kernel_entry) also.
///
/// # Safety
///
/// This function is the kernel entry and should only be called by the
/// bootloader. The bootloader should guarantee that the argument is valid.
///
/// This function should never be called directly.
#[ecraldr_base::kernel_entry]
pub unsafe fn kernel_entry(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
    // Relocate the kernel image.
    unsafe { mem::reloc::relocate_me() };

    // Clear the BSS.
    mem::sections::clear_bss();

    // Initialize the per-CPU data area using the early slot. It maybe used while initializing
    // interrupts and timers.
    percpu::init_bsp_early();

    // Initialize the CPU ID for the BSP.
    mp::init_cpu_id_bsp(hart_id);

    // SAFETY: The bootloader guarantees that the argument is valid.
    let arg_ref = unsafe { arg.as_ref_unchecked() };

    // Perform early platform initialization.
    exarch::init::init_early(arg_ref.plat_arg);

    // Print the hello banner.
    print_hello_banner();
    kprintln!(
        "Kernel entry on BSP(hart_id: {:#x}), arg: {:x?}\n",
        hart_id,
        arg_ref
    );

    // Enable the VMM, and jump to the upper address space.
    mem::init_and_enable_vmm(kernel_entry_with_vmm as *const _, hart_id, arg)
}

/// The later kernel entry function that runs after the VMM is initialized.
///
/// # Safety
///
/// This function should only be called by the [`kernel_entry`] function, via
/// [`mem::init_vmm`], and should never be called directly.
pub unsafe fn kernel_entry_with_vmm(hart_id: usize, arg: *const ecraldr_base::BootArg) -> ! {
    // Relocate the kernel image again.
    unsafe { mem::reloc::relocate_me() };

    // Re-initialize the per-CPU data area pointer after relocation, to fix the per-CPU data area
    // pointer after relocation.
    percpu::init_bsp_after_reloc();

    // Call the relocation hook.
    exarch::reloc_hook::after_reloc();

    // Print the hello banner.
    kprintln!(
        "{HLINE}\necraOS now in VMM world...\n{HLINE}\nVMM enabled on hart_id: {:#x}\n",
        hart_id
    );

    // Initialize logging.
    logging::init();
    info!("Logger initialized");

    // Finish memory initialization after enabling VMM.
    mem::init_after_enable_vmm();

    // Perform later platform initialization.
    let boot_arg = unsafe { arg.as_ref_unchecked() };
    exarch::init::init_later();

    // Probe devices.
    device::probe_devices(boot_arg.plat_arg);

    irq::init();

    external_irq_smoke_test();

    timer::init_bsp();
    timer::smoke_test();

    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());

    kprintln!("\n\nHere we go!\n\n");

    info!("Starting up secondary CPUs...");
    mp::start_secondary_cpus().expect("failed to start secondary CPUs");

    smoke::remote_slab_free_bsp();

    mem::remove_identical_mappings();

    run_sleep_join_workload(true)
}

#[cfg(target_arch = "riscv64")]
static UART_EVENTS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(target_arch = "riscv64")]
static UART_BASE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(target_arch = "riscv64")]
fn handle_uart_irq() {
    let base = UART_BASE.load(core::sync::atomic::Ordering::Acquire);
    // Disable all UART interrupt causes before returning to the PLIC complete path.
    unsafe { (base as *mut u8).add(1).write_volatile(0) };
    UART_EVENTS.fetch_add(1, core::sync::atomic::Ordering::Release);
}

#[cfg(target_arch = "riscv64")]
fn arm_uart_tx_empty() {
    let base = UART_BASE.load(core::sync::atomic::Ordering::Acquire);
    // Enable only the 16550 transmit-holding-register-empty source.
    unsafe { (base as *mut u8).add(1).write_volatile(1 << 1) };
}

#[cfg(target_arch = "riscv64")]
fn external_irq_smoke_test() {
    use core::sync::atomic::Ordering;

    let (irq, uart_phys) =
        exarch::irq::uart_test_source().expect("PLIC UART test source was not discovered");
    let uart = crate::mem::phys_to_virt(uart_phys).as_usize();
    UART_BASE.store(uart, Ordering::Release);
    UART_EVENTS.store(0, Ordering::Release);
    irq::global::register(irq, handle_uart_irq).expect("UART IRQ registration failed");

    arm_uart_tx_empty();
    exarch::irq::enable_local();
    exarch::time::spin_wait_for(exarch::time::Duration::from_millis(20));
    assert_eq!(
        UART_EVENTS.load(Ordering::Acquire),
        0,
        "disabled PLIC UART source delivered: irq={irq:?}"
    );
    unsafe { (uart as *mut u8).add(1).write_volatile(0) };

    irq::global::set_enabled(irq, true).expect("PLIC UART source enable failed");
    for expected in 1..=2 {
        arm_uart_tx_empty();
        let deadline = exarch::time::monotonic_time() + exarch::time::Duration::from_millis(200);
        while UART_EVENTS.load(Ordering::Acquire) < expected
            && exarch::time::monotonic_time() < deadline
        {
            core::hint::spin_loop();
        }
        assert!(
            UART_EVENTS.load(Ordering::Acquire) >= expected,
            "PLIC UART delivery timed out: irq={irq:?}, expected={expected}, observed={}, deadline={deadline:?}",
            UART_EVENTS.load(Ordering::Acquire)
        );
    }
    irq::global::set_enabled(irq, false).expect("PLIC UART source disable failed");
    _ = irq::global::unregister(irq).expect("PLIC UART unregister failed");
    let before = irq::global::unhandled_count(irq);
    irq::global::test_unmask(irq).expect("PLIC UART test source unmask failed");
    arm_uart_tx_empty();
    let deadline = exarch::time::monotonic_time() + exarch::time::Duration::from_millis(200);
    while irq::global::unhandled_count(irq) == before && exarch::time::monotonic_time() < deadline {
        core::hint::spin_loop();
    }
    let after = irq::global::unhandled_count(irq);
    assert!(
        after > before,
        "unregistered UART interrupt was not counted: irq={irq:?}, before={before}, after={after}, deadline={deadline:?}"
    );
    irq::global::test_mask(irq).expect("PLIC UART source could not be masked after self-test");
    unsafe { (uart as *mut u8).add(1).write_volatile(0) };
    info!(
        "PLIC UART self-test completed: irq={irq:?}, handled=2, unhandled={}",
        after - before
    );
}

#[cfg(target_arch = "x86_64")]
static PIT_EVENTS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "x86_64")]
fn handle_pit_irq() {
    PIT_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// Arms PIT channel 0 in one-shot mode for the IOAPIC self-test.
///
/// The divisor gives the self-test enough time to observe delivery without making boot slow.
#[cfg(target_arch = "x86_64")]
fn arm_pit_oneshot() {
    const PIT_DIVISOR: u16 = 20_000;
    unsafe {
        core::arch::asm!("out 0x43, al", in("al") 0x30_u8, options(nomem, nostack, preserves_flags));
        core::arch::asm!("out 0x40, al", in("al") PIT_DIVISOR as u8, options(nomem, nostack, preserves_flags));
        core::arch::asm!("out 0x40, al", in("al") (PIT_DIVISOR >> 8) as u8, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_arch = "x86_64")]
fn wait_for_pit_count(expected: usize, deadline: exarch::time::TimeValue) {
    while PIT_EVENTS.load(Ordering::Acquire) < expected && exarch::time::monotonic_time() < deadline
    {
        core::hint::spin_loop();
    }
}

#[cfg(target_arch = "x86_64")]
fn external_irq_smoke_test() {
    let irq = exarch::irq::legacy_irq_source(0)
        .expect("IOAPIC is not initialized before the PIT self-test");
    irq::global::register(irq, handle_pit_irq).expect("PIT IRQ registration failed");
    let vector = exarch::irq::routed_vector(irq).expect("PIT route has no CPU vector");

    PIT_EVENTS.store(0, Ordering::Release);
    arm_pit_oneshot();
    exarch::irq::enable_local();
    exarch::time::spin_wait_for(exarch::time::Duration::from_millis(30));
    assert_eq!(
        PIT_EVENTS.load(Ordering::Acquire),
        0,
        "masked PIT delivered: gsi={irq:?}, vector={vector:#x}"
    );

    irq::global::set_enabled(irq, true).expect("PIT IRQ enable failed");
    for expected in 1..=2 {
        arm_pit_oneshot();
        let deadline = exarch::time::monotonic_time() + exarch::time::Duration::from_millis(200);
        wait_for_pit_count(expected, deadline);
        assert!(
            PIT_EVENTS.load(Ordering::Acquire) >= expected,
            "PIT delivery timed out: gsi={irq:?}, vector={vector:#x}, expected={expected}, observed={}, deadline={deadline:?}",
            PIT_EVENTS.load(Ordering::Acquire)
        );
    }

    irq::global::set_enabled(irq, false).expect("PIT IRQ disable failed");
    _ = irq::global::unregister(irq).expect("PIT IRQ unregister failed");
    let before = irq::global::unhandled_count(irq);
    irq::global::test_unmask(irq).expect("PIT test route unmask failed");
    arm_pit_oneshot();
    let deadline = exarch::time::monotonic_time() + exarch::time::Duration::from_millis(200);
    while irq::global::unhandled_count(irq) == before && exarch::time::monotonic_time() < deadline {
        core::hint::spin_loop();
    }
    let after = irq::global::unhandled_count(irq);
    assert!(
        after > before,
        "unregistered PIT was not counted: gsi={irq:?}, vector={vector:#x}, before={before}, after={after}, deadline={deadline:?}"
    );
    irq::global::test_mask(irq).expect("PIT source could not be masked after self-test");
    info!(
        "IOAPIC PIT self-test completed: gsi={irq:?}, vector={vector:#x}, handled=2, unhandled={}",
        after - before
    );
}

/// The kernel entry function for the AP.
///
/// # Safety
///
/// This function should never be called directly.
pub unsafe fn kernel_entry_ap(phys_id: usize) -> ! {
    let cpu_id = mp::phys_to_logi_id(phys_id);

    // Initialize the per-CPU data area for the AP.
    percpu::init_ap(cpu_id);

    // Initialize the CPU ID for the AP.
    mp::init_cpu_id_ap(phys_id);

    info!(
        "Kernel entry on AP {}(hart_id: {})",
        mp::current_cpu_id(),
        phys_id
    );

    // Perform early platform initialization for the AP.
    exarch::init::init_early_ap();

    // Initialize memory for the AP.
    mem::init_ap();

    exarch::init::init_later_ap();

    timer::init_ap();

    task::init_scheduler_current_cpu(mp::current_cpu_boot_stack_range());

    mp::mark_ap_up(cpu_id);

    timer::smoke_test();

    smoke::remote_slab_free_ap();

    run_sleep_join_workload(false)
}

/// Minimal panic handler: spin forever with interrupts possibly still disabled.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use exarch::power::{ShutdownReason, shutdown};

    if logging::is_inited() {
        error!("Kernel panic: {}", info);
    } else {
        kprintln!("Kernel panic: {}", info);
    }

    shutdown(ShutdownReason::Panicked)
}
