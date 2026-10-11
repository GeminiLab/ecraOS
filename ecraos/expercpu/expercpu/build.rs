use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=test_percpu.x");
    println!("cargo:rerun-if-changed=tests/test_percpu.rs");

    let target_is_linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if target_is_linux {
        let manifest_dir = Path::new(std::env!("CARGO_MANIFEST_DIR"));
        let test_path = manifest_dir.join("tests/test_percpu.rs");
        if test_path.exists() {
            let ld_script_path = manifest_dir.join("test_percpu.x");
            println!("cargo:rustc-link-arg-tests=-no-pie");
            println!("cargo:rustc-link-arg-tests=-T{}", ld_script_path.display());
        }
    }
}
