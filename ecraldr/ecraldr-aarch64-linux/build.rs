fn main() {
    // The Linux arm64 Image protocol loads this image 2 MiB after RAM starts.
    // A future option may select the maximum supported page size instead.
    ecraldr_build_rs::specify_static_link_script("aarch64-linux", 0x4020_0000, 65536);
}
