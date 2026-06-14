# CLAUDE.md for ecraOS

ecraOS is an experimental and educational operating-system kernel written in
Rust. It aims to stay flexible across architectures, while the current working
target is x86-64.

## Structure

The main crates are:

- `ecraos/` - The PIE-enabled kernel.
- `exarch/` - Architecture-specific support code and architecture-kernel
  interfaces. The current implementation is x86-64.
- `exboot/` - Boot argument definitions, kernel-entry glue, and boot support.
- `ecraos-loader/` - The static loader binary that embeds the stripped kernel.
- `exboot/exboot-multiboot-x86_64/` - The current Multiboot 1 boot module for
  x86-64.

Supporting crates include `expt` for page tables, `exbuddy`/`exslab` for memory
allocation, `expalloc_trait` for page allocation traits, and small utility
crates such as `size-disp` and `memory_range_set`.

## Build & Run Commands

### Build Procedure

The kernel-side platform implementation is no longer built through a separate
`explat_impl` artifact. `exarch` is a normal workspace dependency of `ecraos`,
so the kernel and architecture support code are compiled in one Cargo graph.

The current build procedure is:

1. Build the kernel with PIE relocation and the kernel linker script.
2. Strip the kernel to a flat binary.
3. Build the boot module crate, currently `exboot-multiboot-x86_64`.
4. Build `ecraos-loader` with static relocation, the loader linker script,
   `KERNEL_BIN=<path-to-stripped-kernel>`, and
   `--extern exboot_impl=<path-to-boot-module-rlib>`.
5. Strip the loader and run it in QEMU.

### Build Script

A bash script named `test.sh` builds and runs the kernel in QEMU.

```bash
# Build and run in QEMU (with 8s timeout)
./test.sh

# Clean build then run
CLEAN=1 ./test.sh

# Build and run with release profile
PROFILE=release ./test.sh

# Run with extra QEMU flags
QEMU_EXTRA_ARGS='-d guest_errors' ./test.sh

# Longer timeout
TIMEOUT_SEC=15 ./test.sh
```

There are no automated end-to-end tests beyond the QEMU boot smoke test.

### Fmt and Clippy

Run `cargo fmt` to format the code. Run
`cargo clippy --workspace --target x86_64-unknown-none` to check for lint
errors.

## Code Format

We use the following code format conventions, beyond common Rust conventions:

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
