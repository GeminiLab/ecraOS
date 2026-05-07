#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

PLAT="$ROOT/target/x86_64-unknown-none/debug/libexplat_x86_64_multiboot.rlib"
KERNEL="$ROOT/target/x86_64-unknown-none/debug/ecraos"

cargo clean
cargo build -p explat-x86_64-multiboot --target x86_64-unknown-none
RUSTFLAGS="-C link-arg=-Tlink.ld --cfg link_with_explat_impl --extern explat_impl=$PLAT -C relocation-model=static" cargo build -p ecraos --target x86_64-unknown-none

TIMEOUT_SEC="${TIMEOUT_SEC:-8}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"

set +e
timeout --foreground "${TIMEOUT_SEC}" \
  qemu-system-x86_64 \
  -nographic \
  -no-reboot \
  -kernel "${KERNEL}" \
  ${QEMU_EXTRA_ARGS}
