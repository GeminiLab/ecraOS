#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

printf 'Host test suite\n'
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
printf '%s\n%s\n' "$rustc_version" "$cargo_version"

cargo test --locked \
    --manifest-path "$ROOT/ecraos/Cargo.toml" \
    -p exbuddy \
    -p exslab \
    -p expt \
    -p expercpu \
    -p memory_range_set \
    -p maybe_non_generic \
    -p size_disp \
    -p dyn_static_traits \
    -p expalloc_trait

cargo test --locked \
    --manifest-path "$ROOT/ecraldr/Cargo.toml" \
    -p ecraldr_base \
    -p ecraldr_base_macros

cargo test --locked --manifest-path "$ROOT/ecraos/Cargo.toml" -p ecraos --lib --features host-test

printf 'Host test suite PASS\n'
