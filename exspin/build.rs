//! Linker setup for host tests using actual per-CPU storage.

use std::{env, path::Path};

/// Supplies the existing per-CPU boundary script to this crate's host test executable.
fn main() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../expercpu/expercpu/test_percpu.x");
    println!("cargo:rerun-if-changed={}", script.display());
    if env::var_os("CARGO_FEATURE_HOST_TEST").is_some() {
        println!("cargo:rustc-link-arg=-T{}", script.display());
    }
}
