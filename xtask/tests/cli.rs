//! Command-line interface regression tests.
//!
//! These tests exercise the public executable without starting Cargo builds or QEMU.

use std::process::{Command, Output};

/// Invokes the xtask executable with the supplied arguments.
///
/// Each invocation is isolated from environment options used by local smoke runs.
fn invoke(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    for name in ["CRATE", "TARGET", "PROFILE", "CPUS", "TIMEOUT_SEC", "CLEAN"] {
        command.env_remove(name);
    }
    command.args(args).output().unwrap()
}

/// Describes the three supported task commands.
///
/// Help must expose the migrated operations without a separate matrix subcommand.
#[test]
fn describes_supported_commands() {
    let output = invoke(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for name in ["build", "qemu", "test-host"] {
        assert!(help.contains(name), "missing {name} in {help}");
    }
    assert!(!help.contains("test-matrix"));
}

/// Rejects the removed matrix command.
///
/// Matrix execution belongs to CI and the short local shell wrapper.
#[test]
fn rejects_matrix_command() {
    assert_eq!(invoke(&["test-matrix"]).status.code(), Some(2));
}

/// Rejects unsupported architectures before starting a build.
///
/// A typo in a vector must not silently launch a default architecture.
#[test]
fn rejects_unknown_vector_target() {
    let output = invoke(&["qemu", "--target", "x86_64-unknown-none,invalid-target"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid-target"));
}

/// Rejects invalid CPU vectors before starting a guest.
///
/// Empty entries, zero counts, and the retired bracket syntax are invalid.
#[test]
fn rejects_invalid_cpu_vectors() {
    for cpus in ["", "1,", ",4", "0,4", "1,nope", "[1,4]"] {
        let output = invoke(&["qemu", "--cpus", cpus]);
        assert_eq!(output.status.code(), Some(2), "accepted {cpus:?}");
    }
}
