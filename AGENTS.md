# Global AI instructions for ecraOS

ecraOS is an experimental and educational operating-system kernel written in
Rust. It aims to stay flexible across architectures, while the current working
targets are x86-64, RISC-V 64, and AArch64.

## Structure

The repository has three independent Cargo workspaces: the root kernel workspace,
`ecraldr/`, and `smoke-apps/`.

- `./` - The kernel workspace.
  - `ecraos/` - The PIE-enabled kernel library.
  - `exarch/` - Architecture-specific support code and architecture-kernel
    interfaces for x86-64, RISC-V 64, and AArch64.
- `ecraldr/` - The loader workspace.
  - `ecraldr/ecraldr` - Shared loader code that embeds the stripped kernel.
  - `ecraldr/ecraldr_base` - Boot argument definitions and kernel-entry glue.
  - `ecraldr/ecraldr_base_macros` - Procedural macros for kernel-entry glue.
  - `ecraldr/ecraldr-x86_64-multiboot` - The current Multiboot 1 loader for x86-64.
  - `ecraldr/ecraldr-riscv64-none` - The current RISC-V 64 loader.
  - `ecraldr/ecraldr-aarch64-linux` - The current AArch64 Linux-image loader.
  - `ecraldr/ecraldr_build_rs` - An excluded standalone loader build helper.
- `smoke-apps/` - A Cargo workspace for the independent kernel binaries:
  `default`, `ipi`, `sync-preemption`, and `sleep-join`. Shared dependencies,
  and the lockfile are defined at the workspace root.
- `xtask/` - A standalone host-side Cargo task runner excluded from the kernel
  workspace.

Supporting crates include `expt` for page tables, `exbuddy`/`exslab` for memory
allocation, `expalloc_trait` for page allocation traits, and small utility
crates such as `size_disp` and `memory_range_set`. These supporting crates live
at the repository root beside `ecraos/`.

## Working agreements

- Understand the goal and existing implementation before changing code. Keep changes
  focused and use the existing abstractions where they fit.
- Use semantic address types for memory addresses and document every Rust item.
- Base verification claims on checks that were actually run.

## Load instructions for the current work

Read the relevant skill before the action below. Skills may apply together, and
the applicable set can change during a task. Load their references only as directed.

| Before this action | Read and follow |
| --- | --- |
| Writing, modifying, or reviewing code | [ecra-code](.agents/skills/ecra-code/SKILL.md) |
| Writing, modifying, or reviewing documentation | [ecra-docs](.agents/skills/ecra-docs/SKILL.md) |
| Planning tests, running checks, or reporting work complete | [ecra-verify](.agents/skills/ecra-verify/SKILL.md) |

Code changes that add or change documented behavior also require ecra-docs.
Behavior changes require ecra-verify when choosing checks, before implementation.
Documentation-only work uses ecra-docs and ecra-verify without loading code rules.

## Build & Run Commands

### Build Procedure

The kernel-side platform implementation is no longer built through a separate
`explat_impl` artifact. `exarch` is a normal workspace dependency of `ecraos`,
so the kernel and architecture support code are compiled in one Cargo graph.

The current build procedure is:

1. Build a smoke application with PIE relocation and `ecraos/link.ld`.
2. Strip the kernel to a flat binary.
3. Build the target-specific `ecraldr-*` loader with static relocation, the loader
   linker script, and `KERNEL_BIN=<path-to-stripped-kernel>`.
4. Strip the loader and run it in QEMU.

The smoke applications share one Cargo artifact directory, separate from the
loader's directory. Build tasks obtain each directory from `cargo metadata`,
including when `CARGO_TARGET_DIR` overrides the default locations. QEMU logs
are collected in the repository's `target/qemu-logs/` directory.

### Build and Test Tasks

Use `cargo xtask` from the repository root. The standalone task runner requires
Cargo, `rust-objcopy` from cargo-binutils, and the selected `qemu-system-*`
executable. Output logging and guest timeouts are handled in Rust.

```bash
# Build and run the default application in QEMU (with 15s timeout)
cargo xtask qemu

# Build and run another independent application
cargo xtask qemu --crate smoke-apps/sleep-join

# Clean build then run
cargo xtask qemu --clean

# Build and run with release profile
cargo xtask qemu --profile release

# Run with extra QEMU flags
cargo xtask qemu --qemu-arg=-d --qemu-arg=guest_errors

# Longer timeout
cargo xtask qemu --timeout 30

# Build and print the loader ELF path without starting QEMU
cargo xtask build --target riscv64gc-unknown-none-elf

# Build RustSBI and use it as the RISC-V QEMU firmware
bash scripts/build-rustsbi.sh
cargo xtask qemu --target riscv64gc-unknown-none-elf --cpus 1,4 \
    --qemu-arg=-bios --qemu-arg=target/rustsbi/rustsbi-prototyper-dynamic.bin

# Run both profiles and CPU counts for multiple applications
cargo xtask qemu --crate smoke-apps/default,smoke-apps/sleep-join \
    --profile debug,release --cpus 1,4

# Run host tests from both workspaces
cargo xtask test-host
```

The qemu command accepts scalar or comma-separated values for `--crate`,
`--target`, `--profile`, and `--cpus`, and executes their Cartesian product.
Vectors have no brackets. All runs finish before a vector result table is
printed, including when an earlier run fails. A single run exits with 0 for
success, 1 for guest failure, 2 for build failure, or 124 for timeout. A vector
run exits with 1 if any cell fails.

The `CRATE`, `TARGET`, `PROFILE`, `CPUS`, `TIMEOUT_SEC`, and `CLEAN=1` environment
settings are supported by applicable tasks. Explicit flags take precedence.
Extra QEMU options use repeated `--qemu-arg` flags.

The RustSBI build helper pins the upstream revision and uses this repository's
`nightly-2026-06-01` toolchain with `rust-src`, LLVM tools, and `rust-objcopy`.
Its checkout and firmware artifacts stay under `target/rustsbi/`.
See `docs/analysis/2026-10-11-rustsbi-boot.md` for the startup requirements and validation.

Cleaning is scoped to the selected guest target so shared directories retain
QEMU logs and host-side xtask artifacts. It removes that target's subdirectory
in each Cargo artifact root, while retaining host build-script and proc-macro caches.

Run `scripts/test-matrix.sh` for all three smoke applications across the three
architectures, debug/release profiles, and single/multiple CPUs. The short
wrapper uses qemu vectors and accepts `CRATE` and `MATRIX_TIMEOUT_SEC` overrides.
CI expresses the same 36 combinations in a GitHub Actions matrix and invokes
qemu once per job. A separate `rustsbi-tests` CI job builds the pinned RustSBI
firmware once and uses xtask's QEMU vectors to run all three applications with
debug/release profiles and 1/4 CPUs, covering 12 additional RISC-V combinations.
It uploads firmware build and QEMU logs even when a step fails.
There is no test-matrix xtask command.

### Fmt and Clippy

Run Cargo commands with an explicit manifest path from the repository root:

```bash
cargo fmt --manifest-path Cargo.toml --all
cargo fmt --manifest-path ecraldr/Cargo.toml --all
cargo fmt --manifest-path ecraldr/ecraldr_build_rs/Cargo.toml
cargo fmt --manifest-path xtask/Cargo.toml
cargo fmt --manifest-path smoke-apps/Cargo.toml --all

cargo clippy --manifest-path Cargo.toml --workspace --target x86_64-unknown-none
cargo clippy --manifest-path xtask/Cargo.toml --all-targets
cargo test --manifest-path xtask/Cargo.toml --locked
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr -p ecraldr_base \
    -p ecraldr_base_macros -p ecraldr-x86_64-multiboot --target x86_64-unknown-none
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-riscv64-none \
    --target riscv64gc-unknown-none-elf
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-aarch64-linux \
    --target aarch64-unknown-none-softfloat
```

Each loader binary must be checked for its own architecture. A loader workspace
check with `--workspace` and one target also selects incompatible loader binaries.

The kernel workspace pins the shared `memory_addr` dependency used by its crates.
Smoke applications inherit it through their `ecraos` dependency.

## Code Format

Follow address-type, documentation, and Rust item-ordering rules in
[ecra-code](.agents/skills/ecra-code/SKILL.md), its style reference, and
[ecra-docs](.agents/skills/ecra-docs/SKILL.md). Run lint checks as described by
[ecra-verify](.agents/skills/ecra-verify/SKILL.md).

Paths in this file are relative to the repository root. Links are reading
instructions, not automatic includes. Read applicable directory-level AGENTS.md
files before modifying files in those directories.

See [the maintenance guide](docs/ai/README.md) when changing these instructions.
