//! Host-side test orchestration.
//!
//! Runs the kernel support crates, shared loader interfaces, and kernel host-test suite.

use std::{path::Path, process::Command};

use crate::{TaskResult, process};

/// Runs the existing host test suites.
///
/// Selects explicit manifests because the repository root is not a Cargo workspace.
pub fn test(root: &Path) -> TaskResult<()> {
    for executable in ["rustc", "cargo"] {
        process::run(Command::new(executable).arg("--version"), None)?;
    }
    let mut kernel = process::cargo(root);
    kernel
        .args(["test", "--locked", "--manifest-path"])
        .arg(root.join("ecraos/Cargo.toml"));
    for package in [
        "exbuddy",
        "exslab",
        "expt",
        "expercpu",
        "memory_range_set",
        "maybe_non_generic",
        "size_disp",
        "dyn_static_traits",
        "expalloc_trait",
    ] {
        kernel.args(["-p", package]);
    }
    process::run(&mut kernel, None)?;
    process::run(
        process::cargo(root)
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join("ecraldr/Cargo.toml"))
            .args(["-p", "ecraldr_base", "-p", "ecraldr_base_macros"]),
        None,
    )?;
    process::run(
        process::cargo(root)
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join("ecraos/Cargo.toml"))
            .args(["-p", "ecraos", "--lib", "--features", "host-test"]),
        None,
    )?;
    println!("Host test suite PASS");
    Ok(())
}
