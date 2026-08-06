#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

target=""
profile=""
clean=0

while (($# > 0)); do
    case "$1" in
        --target)
            target="${2:?missing value for --target}"
            shift 2
            ;;
        --profile)
            profile="${2:?missing value for --profile}"
            shift 2
            ;;
        --clean)
            clean=1
            shift
            ;;
        *)
            printf 'error: unknown build-image option: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done

if [[ -z "$target" || -z "$profile" ]]; then
    printf 'usage: %s --target <triple> --profile <debug|release> [--clean]\n' "$0" >&2
    exit 2
fi

case "$profile" in
    debug)
        profile_args=()
        artifact_profile="debug"
        ;;
    release)
        profile_args=(--profile release)
        artifact_profile="release"
        ;;
    *)
        printf 'error: unsupported profile: %s\n' "$profile" >&2
        exit 2
        ;;
esac

case "$target" in
    x86_64-unknown-none)
        loader_crate="ecraldr-x86_64-multiboot"
        kernel_cargo_args=()
        ;;
    riscv64gc-unknown-none-elf)
        loader_crate="ecraldr-riscv64-none"
        kernel_cargo_args=(-Z build-std=core,alloc,compiler_builtins)
        ;;
    *)
        printf 'error: unsupported target: %s\n' "$target" >&2
        exit 2
        ;;
esac

if ((clean)); then
    printf 'Cleaning Cargo artifacts\n' >&2
    cargo clean
fi

artifact_dir="$ROOT/target/$target/$artifact_profile"
kernel="$artifact_dir/ecraos"
kernel_stripped="$artifact_dir/ecraos.bin"
loader="$artifact_dir/$loader_crate"
loader_stripped="$artifact_dir/$loader_crate.bin"

printf 'Building kernel for %s (%s)\n' "$target" "$profile" >&2
env RUSTFLAGS='-C relocation-model=pie -C link-arg=-Tecraos/link.ld -C link-arg=-pie' \
    cargo build "${kernel_cargo_args[@]}" -p ecraos --target "$target" "${profile_args[@]}"

printf 'Stripping kernel\n' >&2
rust-objcopy "$kernel" --strip-all -O binary "$kernel_stripped"

printf 'Building loader %s\n' "$loader_crate" >&2
env KERNEL_BIN="$kernel_stripped" RUSTFLAGS='-C relocation-model=static' \
    cargo build -p "$loader_crate" --target "$target" "${profile_args[@]}"

printf 'Stripping loader\n' >&2
rust-objcopy "$loader" --strip-all -O binary "$loader_stripped"

printf '%s\n' "$loader"
