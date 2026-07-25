use std::{
    env, fs,
    path::{self, PathBuf},
};

const STATIC_LINK_SCRIPT: &str = include_str!("../res/link-static.ld");

fn ensure_in_build_rs() {
    if env::var_os("CARGO_MANIFEST_DIR").is_none() {
        panic!("ecraldr_build_rs must be used in a build.rs file");
    }
}

fn out_dir() -> PathBuf {
    env::var_os("OUT_DIR").expect("OUT_DIR must be set").into()
}

fn abs_out_dir() -> PathBuf {
    path::absolute(out_dir()).expect("failed to get absolute path of OUT_DIR")
}

pub fn specify_static_link_script<K: AsRef<str> + ?Sized>(loader_key: &K, base_phys_addr: usize) {
    ensure_in_build_rs();

    let link_script_content =
        STATIC_LINK_SCRIPT.replace("%BASE_ADDRESS%", &format!("{base_phys_addr:#x}"));

    let out_dir = abs_out_dir();
    let out_path = out_dir.join(format!("ecraldr_{}.ld", loader_key.as_ref()));
    fs::write(&out_path, link_script_content).expect("Failed to write link script");

    println!("cargo::rustc-link-arg-bins=-T{}", out_path.display())
}
