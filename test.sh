#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

# Seconds before SIGKILL (override: TIMEOUT_SEC=10 ./test.sh)
TIMEOUT_SEC="${TIMEOUT_SEC:-8}"

# Set CLEAN=1 to run cargo clean first
if [[ "${CLEAN:-0}" == "1" ]]; then
  cargo clean
fi

cargo build --target x86_64-unknown-none

KERNEL="$ROOT/target/x86_64-unknown-none/debug/ecraos"

# Optional extra QEMU flags (space-separated), e.g.:
#   QEMU_EXTRA='-d guest_errors' ./test.sh
QEMU_EXTRA_ARGS=()
if [[ -n "${QEMU_EXTRA:-}" ]]; then
  read -r -a QEMU_EXTRA_ARGS <<< "${QEMU_EXTRA}"
fi

set +e
timeout --foreground "${TIMEOUT_SEC}" \
  qemu-system-x86_64 \
  -nographic \
  -no-reboot \
  -kernel "${KERNEL}" \
  "${QEMU_EXTRA_ARGS[@]}"
