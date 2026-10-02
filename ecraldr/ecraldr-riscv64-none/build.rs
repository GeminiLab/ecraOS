fn main() {
    // TODO: allow user to specify the base physical address
    ecraldr_build_rs::specify_static_link_script("riscv64-none", 0x80200000, 4096);
}
