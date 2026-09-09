#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

crate_path="${CRATE:-smokes/default}"
target="${TARGET:-x86_64-unknown-none}"
profile="${PROFILE:-debug}"
timeout_seconds="${TIMEOUT_SEC:-15}"
cpus="${CPUS:-1}"

runner_args=(
    --target "$target"
    --profile "$profile"
    --cpus "$cpus"
    --timeout "$timeout_seconds"
    --crate "$crate_path"
)

if [[ "${CLEAN:-0}" == "1" ]]; then
    runner_args+=(--clean)
fi

if [[ -n "${QEMU_EXTRA_ARGS:-}" ]]; then
    read -r -a legacy_qemu_args <<< "$QEMU_EXTRA_ARGS"
    index=0
    while ((index < ${#legacy_qemu_args[@]})); do
        argument="${legacy_qemu_args[$index]}"
        if [[ "$argument" == "-smp" && $((index + 1)) -lt ${#legacy_qemu_args[@]} ]]; then
            cpus="${legacy_qemu_args[$((index + 1))]}"
            runner_args[5]="$cpus"
            index=$((index + 2))
            continue
        fi
        if [[ "$argument" == -smp=* ]]; then
            cpus="${argument#-smp=}"
            runner_args[5]="$cpus"
            index=$((index + 1))
            continue
        fi
        runner_args+=(--qemu-arg "$argument")
        index=$((index + 1))
    done
fi

exec scripts/run-qemu-test.sh "${runner_args[@]}"
