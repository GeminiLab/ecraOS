fn main() {
    // Register the `link_with_explat_impl` cfg flag.
    println!("cargo::rustc-check-cfg=cfg(link_with_explat_impl)");
}
