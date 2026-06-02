# CLAUDE.md for ecraOS

ecraOS is experimental and educational OS kernel written in Rust. It tends to be a cross-platform kernel, but currently based on x86-64.

## Structure

There are 3 main parts:

- `ecraos/` - The kernel source code.
- `explat/` - The platform abstraction layer.
- `exboot/` - The bootloader.

Other crates are dependencies for the main crates.

## Build & Run Commands

### Build Procedure

ecraOS uses a complex multi-stage build procedure:

1. Build the platform implementation crate (only `explat-x86_64` for now).
2. Build the kernel, with `--cfg building_ecraos --extern explat_impl=<path>` to link the output of stage 1. Both stage 1 and stage 2 use PIE compilation and linking.
3. Build the bootloader implementation crate (only `exboot-multiboot-x86_64` for now).
4. Build the loader, with `--cfg building_ecraos_loader --extern exboot_impl=<path>` to link the output of stage 3. And use `KERNEL_BIN=<path>` to include the kernel binary. Both stage 3 and stage 4 use static compilation and linking.

### Build Script

A bash script file named `test.sh` is provided to build and run the kernel in QEMU. See the examples below to learn how to use it.

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

There are no automated tests beyond the QEMU boot smoke test.

### Fmt and Clippy

Run `cargo fmt` to format the code. Run `cargo clippy --workspace --target x86_64-unknown-none` to check for lint errors.

## Code format

We use the following code format conventions, beyond common Rust conventions:

- All items (types, type aliases, functions, modules, etc.) should have their doc comments, regardless of their visibility.
   - All doc comments should have a one-line summary, followed by a blank line, followed by a detailed description.
      - The summary of functions should be a verb phrase with 3rd person singular present tense. E.g. "Initializes the VMM layout ...".
      - The summary of types should be a noun phrase. E.g. "The VMM layout.", "Virtual address space modes.", "A memory region.".
      - The summary of modules should be a noun phrase describing its contents or its role. E.g. "Virtual memory management.", "Infomations about the sections of the kernel binary.".
   - Headings in doc comments start at level 1 ("# Heading"), lower levels are used for sub-headings.
   - Semicolons should be avoided in doc comments.
   - Comments wrap at 100 characters.
- Items in Rust source files should be ordered in the following order, with a blank line between each group:
   - Doc comments.
   - Other outer attributes.
   - Extern crate definitions.
   - Uses from `core` and `alloc`.
   - Uses from other crates.
   - Uses from the current crate (should be a single use statement starting with `use crate::...`).
   - Modules definitions (`pub` and private togeter).
   - Uses from self or sub-modules (one use statement per sub-module, starting with `use submodule_name::...;`/`use self::*`).
      - One exception here: if a use statement is used to resolve the visibility issue of a macro, it should be placed immediately after the macro definition.
   - Other items.
- Uses from the same crate should be merged into a single use statement.

Any violation of these rules should be fixed when checking for lint errors.
