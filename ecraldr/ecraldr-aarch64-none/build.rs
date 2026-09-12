fn main() {
    // QEMU virt loads an AArch64 `-kernel` image at this address.
    ecraldr_build_rs::specify_static_link_script("aarch64-none", 0x4008_0000);
}
