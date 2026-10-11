use std::path::Path;

/// Configures the linker script used by ecraOS host tests.
///
/// The host executable needs the same per-CPU boundary symbols as the kernel,
/// while normal kernel builds continue to use the architecture linker script.
fn main() {
    println!("cargo:rerun-if-changed=host-test.ld");

    if std::env::var_os("CARGO_FEATURE_HOST_TEST").is_some() {
        let linker =
            Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("host-test.ld");
        println!("cargo:rustc-link-arg=-T{}", linker.display());
    }
}
