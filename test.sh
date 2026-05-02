cargo clean
cargo build --target x86_64-unknown-none
qemu-system-x86_64 -nographic -kernel target/x86_64-unknown-none/debug/ecraos
