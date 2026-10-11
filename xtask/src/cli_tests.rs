//! Option parsing and Cartesian-product regression tests.
//!
//! These tests cover the input contract shared by local runs and CI.

use clap::Parser;

use crate::cli::{Cli, Target, Task};

/// Expands all supplied vectors into concrete guest runs.
///
/// Each application, architecture, profile, and CPU count participates in the product.
#[test]
fn expands_comma_separated_vectors() {
    let cli = Cli::try_parse_from([
        "xtask",
        "qemu",
        "--crate",
        "smoke-apps/default,smoke-apps/sleep-join",
        "--target",
        "x86_64-unknown-none,riscv64gc-unknown-none-elf",
        "--profile",
        "debug,release",
        "--cpus",
        "1,2,4",
    ])
    .unwrap();
    let Task::Qemu(args) = cli.task else {
        panic!("expected qemu")
    };
    let cells = args.cells();
    assert_eq!(cells.len(), 24);
    assert_eq!(
        cells[0].build.crate_path.to_str(),
        Some("smoke-apps/default")
    );
    assert_eq!(cells[0].build.target, Target::X86_64);
    assert_eq!(cells[0].cpus.get(), 1);
    assert_eq!(
        cells[23].build.crate_path.to_str(),
        Some("smoke-apps/sleep-join")
    );
    assert_eq!(cells[23].build.target, Target::Riscv64);
    assert_eq!(cells[23].cpus.get(), 4);
}

/// Preserves hyphens in additional QEMU arguments.
///
/// Arguments are passed directly to QEMU without shell expansion or splitting.
#[test]
fn accepts_hyphenated_qemu_arguments() {
    let cli = Cli::try_parse_from([
        "xtask",
        "qemu",
        "--qemu-arg",
        "-d",
        "--qemu-arg",
        "guest_errors",
    ])
    .unwrap();
    let Task::Qemu(args) = cli.task else {
        panic!("expected qemu")
    };
    assert_eq!(args.qemu_args, ["-d", "guest_errors"]);
}

/// Rejects vectors for the scalar build command.
///
/// Multiple artifact builds are performed through the qemu runner instead.
#[test]
fn keeps_build_architecture_scalar() {
    assert!(
        Cli::try_parse_from([
            "xtask",
            "build",
            "--target",
            "x86_64-unknown-none,riscv64gc-unknown-none-elf",
        ])
        .is_err()
    );
}

/// Rejects zero-length timeout budgets.
///
/// Timeout validation happens before any build or process is started.
#[test]
fn rejects_zero_timeout() {
    assert!(Cli::try_parse_from(["xtask", "qemu", "--timeout", "0"]).is_err());
}
