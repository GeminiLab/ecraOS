# Build and test reference

Run commands from the repository root. Use the checked-in scripts as the source
of truth for build details, options, and supported targets.

## Select the scope

| Change | Relevant checks |
| --- | --- |
| Rust code | Format, Clippy, manual project-style review, and affected tests |
| Host-testable logic | Affected crate tests, or the host suite for shared changes |
| Kernel behavior | Host tests where applicable and a relevant QEMU smoke program |
| Architecture, loader, or linking | Build and QEMU smoke on affected targets |
| Shared behavior across architectures | QEMU matrix, with profiles and CPU counts relevant to the change |
| Documentation or instructions only | Links, examples, factual consistency, and applicable file/skill validation |

Select test cases for the behavior being changed. Add focused tests when existing
checks do not cover it. Formatting-only and documentation-only changes do not
require new runtime tests. Explain any narrower scope when a full suite is warranted
but cannot be run.

## Format and lint

Run Cargo commands from the repository root with an explicit manifest path. For Rust changes,
format and check every affected workspace:

```bash
cargo fmt --manifest-path Cargo.toml --all
cargo fmt --manifest-path ecraldr/Cargo.toml --all
cargo fmt --manifest-path ecraldr/ecraldr_build_rs/Cargo.toml
cargo fmt --manifest-path xtask/Cargo.toml
cargo fmt --manifest-path smoke-apps/Cargo.toml --all

cargo fmt --manifest-path Cargo.toml --all -- --check
cargo fmt --manifest-path ecraldr/Cargo.toml --all -- --check
cargo fmt --manifest-path ecraldr/ecraldr_build_rs/Cargo.toml -- --check
cargo fmt --manifest-path xtask/Cargo.toml -- --check
cargo fmt --manifest-path smoke-apps/Cargo.toml --all -- --check
```

Run Clippy for affected packages and their supported targets. The kernel workspace is independent
from the loader workspace. Check each loader binary with its matching architecture target rather
than selecting every loader in one target-specific workspace command.

```bash
# Kernel workspace
cargo clippy --manifest-path Cargo.toml --workspace --target x86_64-unknown-none

# Host-side task runner
cargo clippy --manifest-path xtask/Cargo.toml --all-targets

# x86-64 loader
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr -p ecraldr_base \
    -p ecraldr_base_macros -p ecraldr-x86_64-multiboot --target x86_64-unknown-none

# RISC-V loader
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-riscv64-none \
    --target riscv64gc-unknown-none-elf

# AArch64 loader
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-aarch64-linux \
    --target aarch64-unknown-none-softfloat
```

These are scope examples, not a requirement to run every command for every change. Inspect and
report warnings even when Clippy exits successfully. Formatting and Clippy do not enforce all custom
item-ordering, address-type, or documentation rules.

## Host tests

Use the maintained host-test task for both workspaces, or run a focused package test when suitable:

```bash
cargo xtask test-host
cargo test --manifest-path Cargo.toml -p exbuddy
cargo test --manifest-path Cargo.toml -p ecraos --lib --features host-test
cargo test --manifest-path xtask/Cargo.toml --locked
```

Run relevant doc tests when changing executable Rust documentation examples.

## QEMU smoke tests

`cargo xtask qemu` builds and runs the default application for x86-64 with one CPU and a 15-second
timeout. Select other applications, targets, profiles, CPU counts, and QEMU options with flags:

```bash
cargo xtask qemu
cargo xtask qemu --crate smoke-apps/ipi
cargo xtask qemu --target riscv64gc-unknown-none-elf --cpus 1,4
cargo xtask qemu --profile release --timeout 30
cargo xtask qemu --qemu-arg=-d --qemu-arg=guest_errors

# Run the Cartesian product of the selected applications, profiles, and CPU counts
cargo xtask qemu --crate smoke-apps/default,smoke-apps/sleep-join \
    --profile debug,release --cpus 1,4

# Build and run RustSBI-backed RISC-V guests
bash scripts/build-rustsbi.sh
cargo xtask qemu --target riscv64gc-unknown-none-elf --cpus 1,4 \
    --qemu-arg=-bios --qemu-arg=target/rustsbi/rustsbi-prototyper-dynamic.bin

# Clean the selected guest artifacts before building
cargo xtask qemu --clean
```

The CLI also accepts the `CRATE`, `TARGET`, `PROFILE`, `CPUS`, `TIMEOUT_SEC`, and `CLEAN=1`
environment settings. Explicit flags take precedence. QEMU logs are collected under
`target/qemu-logs/`. A timeout is a failed check, not evidence of a successful boot. Inspect both
the runner result and guest log. Smoke tests cover the exercised scenarios, not all kernel behavior.

Run the maintained architecture/profile/CPU matrix with:

```bash
scripts/test-matrix.sh
CRATE=smoke-apps/sync-preemption scripts/test-matrix.sh
```

The matrix defaults to all three applications across all three architectures, debug/release profiles,
and one/four CPUs. Its per-run timeout defaults to 20 seconds and can be changed with
`MATRIX_TIMEOUT_SEC`.

## Build procedure and prerequisites

The build task builds a smoke application with PIE relocation and `ecraos/link.ld`, strips the kernel
to a flat binary, builds the target-specific loader with static relocation and `KERNEL_BIN` set to
that binary, then strips the loader. `cargo xtask build` prints the loader ELF path without starting
QEMU.

The task runner requires Cargo, `rust-objcopy` from cargo-binutils, and the selected
`qemu-system-*` executable for smoke tests. Use `rust-toolchain.toml` for the required toolchain and
components, and `.github/workflows/ci.yml` for the current CI environment and test entrypoints.
