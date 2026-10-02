#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

crate_path="${CRATE:-smokes/default}"
target=""
profile=""
cpus=""
timeout_seconds=""
clean=0
qemu_extra_args=()

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
        --cpus)
            cpus="${2:?missing value for --cpus}"
            shift 2
            ;;
        --timeout)
            timeout_seconds="${2:?missing value for --timeout}"
            shift 2
            ;;
        --clean)
            clean=1
            shift
            ;;
        --qemu-arg)
            qemu_extra_args+=("${2:?missing value for --qemu-arg}")
            shift 2
            ;;
        *)
            printf 'error: unknown run-qemu-test option: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done

if [[ -z "$target" || -z "$profile" || -z "$cpus" || -z "$timeout_seconds" ]]; then
    printf 'usage: %s [--crate <path>] --target <triple> --profile <debug|release> --cpus <count> --timeout <seconds>\n' "$0" >&2
    exit 2
fi

if ! [[ "$cpus" =~ ^[1-9][0-9]*$ && "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: CPU count and timeout must be positive integers\n' >&2
    exit 2
fi

case "$target" in
    x86_64-unknown-none)
        qemu=(qemu-system-x86_64 -cpu qemu64,+fsgsbase,+x2apic)
        ;;
    riscv64gc-unknown-none-elf)
        qemu=(qemu-system-riscv64 -machine virt)
        ;;
    aarch64-unknown-none-softfloat)
        qemu=(qemu-system-aarch64 -machine virt,gic-version=3 -cpu cortex-a72)
        ;;
    *)
        printf 'error: unsupported target: %s\n' "$target" >&2
        exit 2
        ;;
esac

log_dir="$ROOT/target/qemu-logs"
mkdir -p "$log_dir"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
log_path="$log_dir/${timestamp}-$$-${target}-${profile}-${cpus}.log"

printf 'QEMU log: %s\n' "$log_path" >&2
printf 'Crate: %s, target: %s, profile: %s, CPUs: %s\n' \
    "$crate_path" "$target" "$profile" "$cpus" | tee -a "$log_path"

builder_args=(--crate "$crate_path" --target "$target" --profile "$profile")
if ((clean)); then
    builder_args+=(--clean)
fi

set +e
loader_path="$(scripts/build-image.sh "${builder_args[@]}" 2>>"$log_path")"
build_status=$?
set -e
if ((build_status != 0)); then
    printf 'BUILD FAILURE (status %s), log: %s\n' "$build_status" "$log_path" >&2
    exit 2
fi

printf 'Loader: %s\n' "$loader_path" | tee -a "$log_path"

kernel_image="$loader_path"
if [[ "$target" == "aarch64-unknown-none-softfloat" ]]; then
    kernel_image="${loader_path}.bin"
fi
if [[ ! -f "$kernel_image" ]]; then
    printf 'error: kernel image not found: %s\n' "$kernel_image" >&2
    exit 2
fi
qemu_args=("${qemu[@]}" -smp "$cpus" -nographic -no-reboot -kernel "$kernel_image")
qemu_args+=("${qemu_extra_args[@]}")

set +e
timeout --foreground "$timeout_seconds" "${qemu_args[@]}" 2>&1 | tee -a "$log_path"
qemu_status=${PIPESTATUS[0]}
set -e

if ((qemu_status == 124)); then
    printf 'TIMEOUT after %ss, log: %s\n' "$timeout_seconds" "$log_path" >&2
    exit 124
fi

if rg -qi 'Kernel panic:|fatal:|triple fault|unhandled supervisor .*fault' "$log_path"; then
    printf 'GUEST PANIC/FATAL (QEMU status %s), log: %s\n' "$qemu_status" "$log_path" >&2
    exit 1
fi

if ((qemu_status == 0)); then
    printf 'GUEST SUCCESS (QEMU status %s, no panic/fatal log), log: %s\n' "$qemu_status" "$log_path" >&2
    exit 0
fi

printf 'UNKNOWN GUEST TERMINATION (QEMU status %s), log: %s\n' "$qemu_status" "$log_path" >&2
exit 1
