#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

crates="${CRATE:-smoke-apps/default,smoke-apps/sync-preemption,smoke-apps/sleep-join}"
status=0
for target in x86_64-unknown-none riscv64gc-unknown-none-elf aarch64-unknown-none-softfloat; do
    cpus=1,2
    if [[ "$target" == x86_64-unknown-none ]]; then
        cpus=1,4
    fi
    cargo xtask qemu --crate "$crates" --target "$target" --profile debug,release \
        --cpus "$cpus" --timeout "${MATRIX_TIMEOUT_SEC:-20}" || status=1
done
exit "$status"
