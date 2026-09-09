#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

crate_path="${CRATE:-smokes/default}"

targets=(
    x86_64-unknown-none
    x86_64-unknown-none
    x86_64-unknown-none
    x86_64-unknown-none
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
    riscv64gc-unknown-none-elf
)
profiles=(debug debug release release debug debug release release)
cpus=(1 4 1 4 1 2 1 2)

statuses=()
classifications=()
logs=()

for index in "${!targets[@]}"; do
    target="${targets[$index]}"
    profile="${profiles[$index]}"
    cpu_count="${cpus[$index]}"
    printf '\n=== Cell %s: %s %s CPUs=%s ===\n' "$((index + 1))" "$target" "$profile" "$cpu_count"

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

    statuses+=("$cell_status")
    classifications+=("$classification")
    logs+=("$log_path")
done

printf '\nQEMU smoke result table\n'
printf '%-3s %-32s %-8s %-5s %-14s %-8s %s\n' \
    '#' Target Profile CPUs Result Status Log
for index in "${!targets[@]}"; do
    printf '%-3s %-32s %-8s %-5s %-14s %-8s %s\n' \
        "$((index + 1))" "${targets[$index]}" "${profiles[$index]}" "${cpus[$index]}" \
        "${classifications[$index]}" "${statuses[$index]}" "${logs[$index]}"
done

for cell_status in "${statuses[@]}"; do
    if ((cell_status != 0)); then
        exit 1
    fi
done
