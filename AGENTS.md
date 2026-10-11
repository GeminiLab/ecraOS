# Global AI instructions for ecraOS

ecraOS is an experimental and educational operating-system kernel written in
Rust. It aims to stay flexible across architectures, while the current working
targets are x86-64, RISC-V 64, and AArch64.

## Structure

The repository has two Cargo workspaces and independent smoke applications.
There is no root Cargo manifest.

- `ecraos/` - The kernel workspace.
  - `ecraos/ecraos` - The PIE-enabled kernel library.
  - `ecraos/exarch` - Architecture-specific support code and architecture-kernel
    interfaces for x86-64, RISC-V 64, and AArch64.
- `ecraldr/` - The loader workspace.
  - `ecraldr/ecraldr` - Shared loader code that embeds the stripped kernel.
  - `ecraldr/ecraldr_base` - Boot argument definitions and kernel-entry glue.
  - `ecraldr/ecraldr_base_macros` - Procedural macros for kernel-entry glue.
  - `ecraldr/ecraldr-x86_64-multiboot` - The current Multiboot 1 loader for x86-64.
  - `ecraldr/ecraldr-riscv64-none` - The current RISC-V 64 loader.
  - `ecraldr/ecraldr-aarch64-linux` - The current AArch64 Linux-image loader.
  - `ecraldr/ecraldr_build_rs` - An excluded standalone loader build helper.
- `smoke-apps/` - Independent kernel binaries: `default`, `sync-preemption`,
  and `sleep-join`. Each application has its own manifest and lockfile.

Supporting crates include `expt` for page tables, `exbuddy`/`exslab` for memory
allocation, `expalloc_trait` for page allocation traits, and small utility
crates such as `size_disp` and `memory_range_set`. There may be more crates in
the repository. These supporting crates live under `ecraos/`.

## Build & Run Commands

### Build Procedure

The kernel-side platform implementation is no longer built through a separate
`explat_impl` artifact. `exarch` is a normal workspace dependency of `ecraos`,
so the kernel and architecture support code are compiled in one Cargo graph.

The current build procedure is:

1. Build a smoke application with PIE relocation and `ecraos/ecraos/link.ld`.
2. Strip the kernel to a flat binary.
3. Build the target-specific `ecraldr-*` loader with static relocation, the loader
   linker script, and `KERNEL_BIN=<path-to-stripped-kernel>`.
4. Strip the loader and run it in QEMU.

The application and loader have separate Cargo artifact directories. Build
scripts obtain each directory from `cargo metadata`, including when
`CARGO_TARGET_DIR` overrides the default locations. QEMU logs are collected in
the repository's `target/qemu-logs/` directory.

### Build Script

A bash script named `test.sh` builds and runs the kernel in QEMU.

```bash
# Build and run the default application in QEMU (with 15s timeout)
./test.sh

# Build and run another independent application
CRATE=smoke-apps/sleep-join ./test.sh

# Clean build then run
CLEAN=1 ./test.sh

# Build and run with release profile
PROFILE=release ./test.sh

# Run with extra QEMU flags
QEMU_EXTRA_ARGS='-d guest_errors' ./test.sh

# Longer timeout
TIMEOUT_SEC=30 ./test.sh
```

Run `scripts/test-host.sh` for the host tests in both workspaces. Run
`scripts/test-matrix.sh` for all three smoke applications across the three
architectures, debug/release profiles, and single/multiple CPUs. Set `CRATE` to
run this matrix for one application.

### Fmt and Clippy

Run Cargo commands with an explicit manifest path from the repository root:

```bash
cargo fmt --manifest-path ecraos/Cargo.toml --all
cargo fmt --manifest-path ecraldr/Cargo.toml --all
cargo fmt --manifest-path ecraldr/ecraldr_build_rs/Cargo.toml
for app in smoke-apps/*; do
    cargo fmt --manifest-path "$app/Cargo.toml"
done

cargo clippy --manifest-path ecraos/Cargo.toml --workspace --target x86_64-unknown-none
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr -p ecraldr_base \
    -p ecraldr_base_macros -p ecraldr-x86_64-multiboot --target x86_64-unknown-none
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-riscv64-none \
    --target riscv64gc-unknown-none-elf
cargo clippy --manifest-path ecraldr/Cargo.toml -p ecraldr-aarch64-linux \
    --target aarch64-unknown-none-softfloat
```

Each loader binary must be checked for its own architecture. A loader workspace
check with `--workspace` and one target also selects incompatible loader binaries.

The smoke applications must repeat the workspace's `memory_addr` patch because
Cargo only applies patches from the root manifest of the current build.

## Code Format

We use the following code format conventions, beyond common Rust conventions:

- Memory addresses should use semantic address types such as `VirtAddr`,
  `PhysAddr`, `VirtAddrRange`, and `PhysAddrRange` whenever possible.
  - Raw `usize` should be used for sizes, counts, indices, offsets, alignment
    values, bit patterns, and unavoidable pointer or atomic storage boundaries.
  - Public APIs should not accept or return `usize` when the value is a virtual
    or physical address.
  - Prefer `va!` and `pa!` for integer-to-address conversions, and prefer
    `VirtAddr` pointer conversion helpers over raw pointer/integer casts.
  - Conversions between semantic address types and `usize` should stay close to
    raw pointer dereferences, inline assembly, atomic pointer storage, or other
    low-level boundaries that require an integer representation.
- All items (types, type aliases, functions, modules, etc.) should have doc
  comments, regardless of their visibility.
  - All doc comments should have a one-line summary, followed by a blank line,
    followed by a detailed description.
  - Function summaries should use a third-person singular present-tense verb
    phrase. For example: "Initializes the VMM layout ...".
  - Type summaries should use a noun phrase. For example: "The VMM layout.",
    "Virtual address space modes.", "A memory region.".
  - Module summaries should use a noun phrase describing contents or role. For
    example: "Virtual memory management.", "Information about the kernel binary
    sections.".
  - Headings in doc comments start at level 1 (`# Heading`), with lower levels
    used for subheadings.
  - Semicolons should be avoided in doc comments.
  - Comments wrap at 100 characters.
- Items in Rust source files should be ordered in the following order, with a
  blank line between each group:
  - Doc comments.
  - Other outer attributes.
  - Extern crate definitions.
  - Uses from `core` and `alloc`.
  - Uses from other crates.
  - Uses from the current crate (a single use statement starting with
    `use crate::...`).
  - Module definitions (`pub` and private together).
  - Uses from self or submodules (one use statement per submodule, starting
    with `use submodule_name::...;` or `use self::*`).
  - Other items.
- One exception to the ordering rule: if a use statement resolves macro
  visibility, it should be placed immediately after the macro definition.
- Uses from the same crate should be merged into a single use statement.

Any violation of these rules should be fixed when checking for lint errors.
