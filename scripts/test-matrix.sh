#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -n "${CRATE:-}" ]]; then
    smoke_crates=("$CRATE")
else
    smoke_crates=(
        smokes/default
        smokes/sync-preemption
        smokes/sleep-join
    )
fi

targets=(
    x86_64-unknown-none
    x86_64-unknown-none
    x86_64-unknown-none
    x86_64-unknown-none
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
    aarch64-unknown-none-softfloat
    aarch64-unknown-none-softfloat
    aarch64-unknown-none-softfloat
    aarch64-unknown-none-softfloat
)
profiles=(debug debug release release debug debug release release debug debug release release)
cpus=(1 4 1 4 1 2 1 2 1 2 1 2)

smoke_names=()
matrix_targets=()
matrix_profiles=()
matrix_cpus=()
statuses=()
classifications=()
logs=()

cell=0
for crate_path in "${smoke_crates[@]}"; do
    smoke_name="${crate_path##*/}"
    for index in "${!targets[@]}"; do
        target="${targets[$index]}"
        profile="${profiles[$index]}"
        cpu_count="${cpus[$index]}"
        cell=$((cell + 1))
        printf '\n=== Cell %s: %s %s %s CPUs=%s ===\n' \
            "$cell" "$smoke_name" "$target" "$profile" "$cpu_count"

        set +e
        output="$(scripts/run-qemu-test.sh \
            --crate "$crate_path" \
            --target "$target" \
            --profile "$profile" \
            --cpus "$cpu_count" \
            --timeout "${MATRIX_TIMEOUT_SEC:-20}" 2>&1)"
        cell_status=$?
        set -e
        printf '%s\n' "$output"

        log_path="$(printf '%s\n' "$output" | rg -o 'target/qemu-logs/[^ ]+\.log' | tail -1 || true)"
        if ((cell_status == 0)); then
            classification=PASS
        elif ((cell_status == 124)); then
            classification=TIMEOUT
        elif printf '%s\n' "$output" | rg -q 'BUILD FAILURE'; then
            classification=BUILD_FAILURE
        else
            classification=FAIL
        fi

        smoke_names+=("$smoke_name")
        matrix_targets+=("$target")
        matrix_profiles+=("$profile")
        matrix_cpus+=("$cpu_count")
        statuses+=("$cell_status")
        classifications+=("$classification")
        logs+=("$log_path")
    done
done

printf '\nQEMU smoke result table\n'
printf '%-3s %-20s %-32s %-8s %-5s %-14s %-8s %s\n' \
    '#' Smoke Target Profile CPUs Result Status Log
for index in "${!smoke_names[@]}"; do
    printf '%-3s %-20s %-32s %-8s %-5s %-14s %-8s %s\n' \
        "$((index + 1))" "${smoke_names[$index]}" "${matrix_targets[$index]}" \
        "${matrix_profiles[$index]}" "${matrix_cpus[$index]}" "${classifications[$index]}" \
        "${statuses[$index]}" "${logs[$index]}"
done

for cell_status in "${statuses[@]}"; do
    if ((cell_status != 0)); then
        exit 1
    fi
done
