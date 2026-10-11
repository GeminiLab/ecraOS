#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

crates="${CRATE:-smoke-apps/default,smoke-apps/sync-preemption,smoke-apps/sleep-join}"
targets="${TARGETS:-x86_64-unknown-none,riscv64gc-unknown-none-elf,aarch64-unknown-none-softfloat}"
cpus="${CPUS:-1,4}"

cargo xtask qemu --crate "$crates" --target "$targets" --profile debug,release \
    --cpus "$cpus" --timeout "${MATRIX_TIMEOUT_SEC:-20}" 
