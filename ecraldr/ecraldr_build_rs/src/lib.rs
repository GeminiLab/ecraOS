use std::{
    env, fs,
    path::{self, PathBuf},
};

const STATIC_LINK_SCRIPT: &str = include_str!("../res/link-static.ld");

/// The minimum supported alignment for page-backed sections.
const MIN_SECTION_ALIGN: usize = 4096;

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

/// Generates a static linker script for a loader.
///
/// `section_align` is the alignment in bytes for page-backed sections. It must
/// be at least 4 KiB and a power of two.
pub fn specify_static_link_script<K: AsRef<str> + ?Sized>(
    loader_key: &K,
    base_phys_addr: usize,
    section_align: usize,
) {
    ensure_in_build_rs();
    validate_section_align(section_align);

    let link_script_content = STATIC_LINK_SCRIPT
        .replace("%BASE_ADDRESS%", &format!("{base_phys_addr:#x}"))
        .replace("%SECTION_ALIGN%", &format!("{section_align:#x}"));

    let out_dir = abs_out_dir();
    let out_path = out_dir.join(format!("ecraldr_{}.ld", loader_key.as_ref()));
    fs::write(&out_path, link_script_content).expect("Failed to write link script");

    println!("cargo::rustc-link-arg-bins=-T{}", out_path.display())
}

/// Validates a page-backed section alignment.
fn validate_section_align(section_align: usize) {
    assert!(
        section_align >= MIN_SECTION_ALIGN,
        "section alignment must be at least 4 KiB"
    );
    assert!(
        section_align.is_power_of_two(),
        "section alignment must be a power of two"
    );
}

#[cfg(test)]
mod tests {
    /// Accepts page-aligned power-of-two values.
    #[test]
    fn accepts_page_aligned_power_of_two_values() {
        super::validate_section_align(4096);
        super::validate_section_align(65536);
    }

    /// Rejects values below the minimum page alignment.
    #[test]
    #[should_panic(expected = "section alignment must be at least 4 KiB")]
    fn rejects_values_below_page_alignment() {
        super::validate_section_align(2048);
    }

    /// Rejects values that are not powers of two.
    #[test]
    #[should_panic(expected = "section alignment must be a power of two")]
    fn rejects_non_power_of_two_values() {
        super::validate_section_align(12288);
    }
}
