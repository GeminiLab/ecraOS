#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

CLEAN="${CLEAN:-0}"

TARGET="x86_64-unknown-none"
PROFILE="${PROFILE:-debug}"
if [ "$PROFILE" = "debug" ]; then
  PROFILE_ARG=""
else
  PROFILE_ARG="--profile $PROFILE"
fi

KERNEL="$ROOT/target/$TARGET/$PROFILE/ecraos"
KERNEL_STRIPPED="$ROOT/target/$TARGET/$PROFILE/ecraos.bin"
BOOT="$ROOT/target/$TARGET/$PROFILE/libexboot_multiboot_x86_64.rlib"
LOADER="$ROOT/target/$TARGET/$PROFILE/ecraos-loader"
LOADER_STRIPPED="$ROOT/target/$TARGET/$PROFILE/ecraos-loader.bin"

if [ "$CLEAN" = "1" ]; then
  echo "Cleaning up..." >&2
  cargo clean
fi

echo "Building ecraos..." >&2
RUSTFLAGS="-C relocation-model=pie -C link-arg=-Tecraos/link.ld" cargo build -p ecraos --target $TARGET $PROFILE_ARG

echo "Stripping ecraos..." >&2
rust-objcopy "$KERNEL" --strip-all -O binary "$KERNEL_STRIPPED"

echo "Building exboot-multiboot-x86_64..." >&2
RUSTFLAGS="-C relocation-model=static" cargo build -p exboot-multiboot-x86_64 --target $TARGET $PROFILE_ARG

echo "Building ecraos-loader..." >&2
KERNEL_BIN="$KERNEL_STRIPPED" RUSTFLAGS="-C relocation-model=static -C link-arg=-Tecraos-loader/link.ld --cfg building_ecraos_loader --extern exboot_impl=$BOOT" cargo build -p ecraos-loader --target $TARGET $PROFILE_ARG

echo "Stripping ecraos-loader..." >&2
rust-objcopy "$LOADER" --strip-all -O binary "$LOADER_STRIPPED"

NORUN="${NORUN:-0}"

if [ "$NORUN" = "1" ]; then
  echo "Skipping run..." >&2
  exit 0
fi

TIMEOUT_SEC="${TIMEOUT_SEC:-8}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"

set +e
timeout --foreground "${TIMEOUT_SEC}" \
  qemu-system-x86_64 \
  -nographic \
  -no-reboot \
  -kernel "${LOADER}" \
  ${QEMU_EXTRA_ARGS}
