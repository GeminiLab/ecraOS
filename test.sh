#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

TARGET="x86_64-unknown-none"

PLAT="$ROOT/target/$TARGET/debug/libexplat_x86_64.rlib"
KERNEL="$ROOT/target/$TARGET/debug/ecraos"
KERNEL_STRIPPED="$ROOT/target/$TARGET/debug/ecraos.bin"
BOOT="$ROOT/target/$TARGET/debug/libexboot_multiboot_x86_64.rlib"
LOADER="$ROOT/target/$TARGET/debug/ecraos-loader"
LOADER_STRIPPED="$ROOT/target/$TARGET/debug/ecraos-loader.bin"

echo "Cleaning up..." >&2
cargo clean

echo "Building explat-x86_64..." >&2
RUSTFLAGS="-C relocation-model=pie" cargo build -p explat-x86_64 --target $TARGET

echo "Building ecraos..." >&2
RUSTFLAGS="-C relocation-model=pie -C link-arg=-Tecraos/link.ld --cfg building_ecraos --extern explat_impl=$PLAT" cargo build -p ecraos --target $TARGET

echo "Stripping ecraos..." >&2
rust-objcopy "$KERNEL" --strip-all -O binary "$KERNEL_STRIPPED"

echo "Building exboot-multiboot-x86_64..." >&2
RUSTFLAGS="-C relocation-model=static" cargo build -p exboot-multiboot-x86_64 --target $TARGET

echo "Building ecraos-loader..." >&2
KERNEL_BIN="$KERNEL_STRIPPED" RUSTFLAGS="-C relocation-model=static -C link-arg=-Tecraos-loader/link.ld --cfg building_ecraos_loader --extern exboot_impl=$BOOT" cargo build -p ecraos-loader --target $TARGET

echo "Stripping ecraos-loader..." >&2
rust-objcopy "$LOADER" --strip-all -O binary "$LOADER_STRIPPED"

TIMEOUT_SEC="${TIMEOUT_SEC:-8}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"

set +e
timeout --foreground "${TIMEOUT_SEC}" \
  qemu-system-x86_64 \
  -nographic \
  -no-reboot \
  -kernel "${LOADER}" \
  ${QEMU_EXTRA_ARGS}
