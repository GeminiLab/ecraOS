fn main() {
    // Register the `building_ecraos` cfg flag.
    println!("cargo::rustc-check-cfg=cfg(building_ecraos)");
}
