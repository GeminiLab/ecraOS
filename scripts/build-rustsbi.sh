#!/usr/bin/env bash
# Builds the pinned RustSBI Prototyper dynamic firmware for QEMU virt.
#
# Keeps upstream sources and artifacts under target/rustsbi and prints the firmware path.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rustsbi_dir="$root/target/rustsbi"
source_dir="$rustsbi_dir/source"
build_dir="$rustsbi_dir/build"
revision=c11e9d2c7850cc9672329e8a4f372214bd31d090
toolchain=nightly-2026-06-01

if [[ ! -e "$source_dir/.git" ]]; then
    git init "$source_dir" >&2
fi
if ! git -C "$source_dir" rev-parse --verify HEAD >/dev/null 2>&1; then
    git -C "$source_dir" fetch --depth 1 https://github.com/rustsbi/rustsbi.git "$revision" >&2
    git -C "$source_dir" checkout --detach FETCH_HEAD >&2
fi
if [[ "$(git -C "$source_dir" rev-parse HEAD)" != "$revision" ]] ||
    [[ -n "$(git -C "$source_dir" status --porcelain)" ]]; then
    echo "Expected a clean RustSBI checkout at $revision in $source_dir" >&2
    exit 1
fi

(
    cd "$source_dir"
    export RUSTUP_TOOLCHAIN="$toolchain"
    export CARGO_TARGET_DIR="$build_dir"
    cargo prototyper build dynamic >&2
)

firmware="$rustsbi_dir/rustsbi-prototyper-dynamic.bin"
cp "$build_dir/riscv64gc-unknown-none-elf/release/rustsbi-prototyper-dynamic.bin" "$firmware"
printf '%s\n' "$firmware"
