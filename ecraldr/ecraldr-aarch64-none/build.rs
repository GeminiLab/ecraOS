fn main() {
    // QEMU virt loads an AArch64 `-kernel` image at this address.
    // A future option may select the maximum supported page size instead.
    ecraldr_build_rs::specify_static_link_script("aarch64-none", 0x4008_0000, 65536);
}
