fn main() {
    println!("cargo::rustc-check-cfg=cfg(ecraos_doc)");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../expercpu/expercpu/test_percpu.x");
    println!("cargo:rerun-if-changed={}", script.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-T{}", script.display());
    }
}
