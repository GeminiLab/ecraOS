//! Build configuration for architecture-inclusive documentation.

/// Registers the custom configuration flag used to expose all architecture modules to rustdoc.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(ecraos_doc)");
}
