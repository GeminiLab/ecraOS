#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

crate_path="${CRATE:-smokes/default}"
target=""
profile=""
clean=0

while (($# > 0)); do
    case "$1" in
        --crate)
            crate_path="${2:?missing value for --crate}"
            shift 2
            ;;
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
    printf 'usage: %s [--crate <path>] --target <triple> --profile <debug|release> [--clean]\n' "$0" >&2
    exit 2
fi

if [[ "$crate_path" = /* ]]; then
    crate_dir="$crate_path"
else
    crate_dir="$ROOT/$crate_path"
fi
crate_manifest="$crate_dir/Cargo.toml"

if [[ ! -f "$crate_manifest" ]]; then
    printf 'error: crate manifest not found: %s\n' "$crate_manifest" >&2
    exit 2
fi

crate_dir="$(cd "$crate_dir" && pwd -P)"
crate_manifest="$crate_dir/Cargo.toml"
metadata="$(cargo metadata --no-deps --format-version 1 --manifest-path "$crate_manifest")"
kernel_name="$(
    jq -r --arg manifest "$crate_manifest" '
        [
            .packages[]
            | select(.manifest_path == $manifest)
            | .targets[]
            | select(.kind | index("bin"))
            | .name
        ]
        | if length == 1 then .[0] else empty end
    ' <<<"$metadata"
)"

if [[ -z "$kernel_name" ]]; then
    printf 'error: crate must define exactly one binary target: %s\n' "$crate_manifest" >&2
    exit 2
fi

target_dir="$(jq -r '.target_directory' <<<"$metadata")"

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
        loader_cargo_args=()
        ;;
    riscv64gc-unknown-none-elf)
        loader_crate="ecraldr-riscv64-none"
        kernel_cargo_args=(-Z build-std=core,alloc,compiler_builtins)
        loader_cargo_args=(-Z build-std=core,alloc,compiler_builtins)
        ;;
    aarch64-unknown-none-softfloat)
        loader_crate="ecraldr-aarch64-none"
        kernel_cargo_args=(-Z build-std=core,alloc,compiler_builtins)
        loader_cargo_args=(-Z build-std=core,alloc,compiler_builtins)
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

artifact_dir="$target_dir/$target/$artifact_profile"
kernel="$artifact_dir/$kernel_name"
kernel_stripped="$artifact_dir/$kernel_name.bin"
loader="$artifact_dir/$loader_crate"
loader_stripped="$artifact_dir/$loader_crate.bin"

printf 'Building kernel crate %s for %s (%s)\n' "$crate_path" "$target" "$profile" >&2
env RUSTFLAGS='-C relocation-model=pie -C link-arg=-Tecraos/link.ld -C link-arg=-pie' \
    cargo build "${kernel_cargo_args[@]}" --manifest-path "$crate_manifest" \
    --target "$target" "${profile_args[@]}"

printf 'Stripping kernel\n' >&2
rust-objcopy "$kernel" --strip-all -O binary "$kernel_stripped"

printf 'Building loader %s\n' "$loader_crate" >&2
env KERNEL_BIN="$kernel_stripped" RUSTFLAGS='-C relocation-model=static' \
    cargo build -p "$loader_crate" --target "$target" "${loader_cargo_args[@]}" "${profile_args[@]}"

printf 'Stripping loader\n' >&2
rust-objcopy "$loader" --strip-all -O binary "$loader_stripped"

printf '%s\n' "$loader"
