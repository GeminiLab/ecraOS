#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

CLEAN="${CLEAN:-0}"

TARGET=${TARGET:-"x86_64-unknown-none"}
PROFILE="${PROFILE:-debug}"
if [ "$PROFILE" = "debug" ]; then
  PROFILE_ARG=""
else
  PROFILE_ARG="--profile $PROFILE"
fi

case "$TARGET" in
  "x86_64-unknown-none")
    ECRALDR_CRATE="ecraldr-x86_64-multiboot"
    QEMU="qemu-system-x86_64"
    KERNEL_CARGO_ARGS=()
    QEMU_DEFAULT_ARGS=(-cpu qemu64,+fsgsbase,+x2apic)
    ;;
  "riscv64gc-unknown-none-elf")
    ECRALDR_CRATE="ecraldr-riscv64-none"
    QEMU="qemu-system-riscv64"
    KERNEL_CARGO_ARGS=(-Z build-std=core,alloc,compiler_builtins)
    QEMU_DEFAULT_ARGS=(-machine virt)
    ;;
  *)
    echo "Unsupported target: $TARGET" >&2
    exit 1
    ;;
esac

KERNEL="$ROOT/target/$TARGET/$PROFILE/ecraos"
KERNEL_STRIPPED="$ROOT/target/$TARGET/$PROFILE/ecraos.bin"
LOADER="$ROOT/target/$TARGET/$PROFILE/$ECRALDR_CRATE"
LOADER_STRIPPED="$ROOT/target/$TARGET/$PROFILE/$ECRALDR_CRATE.bin"

if [ "$CLEAN" = "1" ]; then
  echo "Cleaning up..." >&2
  cargo clean
fi

echo "Building ecraos..." >&2
RUSTFLAGS="-C relocation-model=pie -C link-arg=-Tecraos/link.ld -C link-arg=-pie" \
  cargo build "${KERNEL_CARGO_ARGS[@]}" -p ecraos --target $TARGET $PROFILE_ARG

echo "Stripping ecraos..." >&2
rust-objcopy "$KERNEL" --strip-all -O binary "$KERNEL_STRIPPED"

echo "Building $ECRALDR_CRATE..." >&2
KERNEL_BIN="$KERNEL_STRIPPED" \
  RUSTFLAGS="-C relocation-model=static" \
  cargo build -p $ECRALDR_CRATE --target $TARGET $PROFILE_ARG

echo "Stripping $ECRALDR_CRATE..." >&2
rust-objcopy "$LOADER" --strip-all -O binary "$LOADER_STRIPPED"

NORUN="${NORUN:-0}"

if [ "$NORUN" = "1" ]; then
  echo "Skipping run..." >&2
  exit 0
fi

TIMEOUT_SEC="${TIMEOUT_SEC:-15}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"

set +e
timeout --foreground "${TIMEOUT_SEC}" \
  $QEMU \
  "${QEMU_DEFAULT_ARGS[@]}" \
  -nographic \
  -no-reboot \
  -kernel "${LOADER}" \
  ${QEMU_EXTRA_ARGS}
