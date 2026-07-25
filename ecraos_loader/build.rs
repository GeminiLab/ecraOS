use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{self, PathBuf},
};

fn main() {
    // Register the `building_ecraos_loader` cfg flag.
    println!("cargo::rustc-check-cfg=cfg(building_ecraos_loader)");
    println!("cargo::rerun-if-env-changed=KERNEL_BIN");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is not set");
    let out_dir = PathBuf::from(&out_dir);
    let mut out_file = File::create(out_dir.join("kernel.rs")).expect("Failed to create kernel.rs");

    let (kernel_len, kernel_content) = if let Ok(kernel_path) = env::var("KERNEL_BIN") {
        let kernel_abs_path =
            path::absolute(&kernel_path).expect("Failed to get absolute path of kernel");
        let kernel_len = fs::metadata(&kernel_abs_path)
            .expect("Failed to get metadata of kernel")
            .len();

        println!("cargo::rerun-if-changed={}", kernel_abs_path.display());

        (
            kernel_len,
            format!("*include_bytes!(\"{}\")", kernel_abs_path.display()),
        )
    } else {
        // use a dummy payload if no kernel path is provided
        (4, "[0xde, 0xad, 0xbe, 0xef]".to_string())
    };

    write!(
        out_file,
        r#"
/// The kernel binary (the PIE part).
#[unsafe(link_section = ".kernel")]
#[used(compiler)]
#[used(linker)]
static KERNEL: [u8; {kernel_len}] = {kernel_content};
    "#
    )
    .expect("Failed to write kernel.rs");
}
