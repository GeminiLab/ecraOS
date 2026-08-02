# From `exboot` and `ecraos_loader` to `ecraldr`

Date: 2026-08-02

## Summary

The loader side of ecraOS has been reorganized from a mixed
`exboot`/`ecraos_loader` style layout into the `ecraldr` family of crates.

The current structure is:

```text
ecraldr/
  ecraldr/                       shared loader code
  ecraldr_base/                  kernel-entry ABI and boot arguments
  ecraldr_base_macros/           kernel-entry procedural macros
  ecraldr-x86_64-multiboot/      x86-64 Multiboot 1 loader binary
  ecraldr-riscv64-none/          RISC-V 64 direct loader binary
  ecraldr_build_rs/              build-script helper for loader link scripts
```

This is more than a directory cleanup. It separates loader responsibilities by
their actual role, removes an awkward boot-module boundary, and makes the build
procedure match the runtime model: each architecture boots through a real
target-specific loader binary, while shared loader code and the kernel-entry ABI
remain common.

## Previous Shape

Before the refactor, loader-side responsibilities were spread across names that
described different historical layers:

- `ecraos_loader` contained the static loader binary that embedded the stripped
  PIE kernel.
- `ecraos_boot` contained the data structures and macros used to call the kernel
  entry point.
- architecture-specific boot crates lived under the boot namespace, even though
  they were already becoming concrete loader entry paths.
- the build script had to assemble a loader from a shared binary crate plus an
  externally built boot module artifact.

The old procedure made sense while the loader was a thin shell around one
x86-64 boot path. It became less natural once RISC-V support was added and the
loader became the place where target-specific entry code, boot-argument
normalization, kernel embedding, boot stack placement, and linker-script
selection all met.

The most important smell was that "boot" and "loader" meant different things in
different places:

- some crates defined the contract between the loader and kernel;
- some crates contained the architecture entry code;
- one crate embedded the kernel payload and owned common loader sections;
- build logic had its own implicit role, but no clear crate boundary.

That made the structure harder to reason about than the code itself.

## New Crate Boundaries

The new `ecraldr` layout uses one directory for the whole loader side and gives
each crate a narrow responsibility.

### `ecraldr_base`

`ecraldr_base` is the stable ABI between loader code and the kernel. It defines
the boot stack size, the platform boot argument, the loader range passed to the
kernel, and the expected kernel-entry function signature.

This crate is used by both sides:

- loader binaries construct `BootArg` values and call the kernel entry;
- `ecraos` marks its entry point and receives `BootArg`;
- `exarch` consumes the platform boot argument during early architecture setup.

Renaming it from `ecraos_boot` to `ecraldr_base` matters because the crate is not
an architecture boot module. It is the base interface of the loader stack. The
new name makes it clear that the crate belongs to the loader family while still
being shared with the kernel.

### `ecraldr_base_macros`

`ecraldr_base_macros` contains the procedural macros for the same ABI:

- `#[kernel_entry]` exports the kernel entry symbol;
- `call_kernel_entry!` imports that symbol from the loader side and jumps to it.

Keeping this as a separate proc-macro crate follows Rust's crate model, while
the matching name keeps the macro implementation tied to the ABI crate it
serves.

### `ecraldr`

`ecraldr` is the shared no-std loader crate. It owns the common boot stack,
includes the stripped kernel payload produced by the kernel build, and provides
the loader-stage panic handler macro.

The key build-time input is still:

```text
KERNEL_BIN=<path-to-stripped-kernel>
```

Its build script converts that path into a generated `kernel.rs` file containing
an aligned static kernel payload in the `.kernel` section. That behavior is
common to x86-64 and RISC-V, so it belongs in one shared crate instead of being
duplicated by platform loaders.

### `ecraldr-x86_64-multiboot`

`ecraldr-x86_64-multiboot` is the concrete x86-64 loader binary. It owns the
Multiboot 1 header and assembly entry path, prepares paging state, converts the
Multiboot information pointer into `PlatformBootArg::Multiboot`, computes the
loader range from linker symbols, and calls the kernel entry.

The package name intentionally keeps its hyphenated form. Cargo package names
are command-facing names, and `cargo build -p ecraldr-x86_64-multiboot` reads as
the target-specific loader artifact.

### `ecraldr-riscv64-none`

`ecraldr-riscv64-none` is the concrete RISC-V 64 loader binary. It owns the
direct boot assembly entry path, forwards the runtime hart ID, converts the
device tree pointer into `PlatformBootArg::DeviceTree`, computes the loader
range, and calls the same kernel entry ABI.

Keeping it parallel to the x86-64 loader avoids pretending that both
architectures have the same firmware contract. They share the kernel-entry ABI
and kernel embedding behavior, but the first instructions and platform argument
sources are architecture-specific.

### `ecraldr_build_rs`

`ecraldr_build_rs` is a host-side build-script helper. It writes a concrete
static loader linker script from a template and injects the corresponding
`-T<path>` linker argument for loader binaries.

It is intentionally excluded from normal workspace membership:

```toml
exclude = [
    "ecraldr/ecraldr_build_rs",
]
```

The crate is still available as a build-dependency of the loader binaries, but
it is not treated as a normal no-std target crate during workspace-wide
operations.

## Build Procedure After the Refactor

The current `test.sh` flow is direct:

1. build `ecraos` as a PIE kernel with the kernel linker script;
2. strip the kernel into a flat binary;
3. build the target-specific `ecraldr-*` binary with
   `KERNEL_BIN=<path-to-stripped-kernel>`;
4. strip the loader and pass it to QEMU.

For x86-64, the selected loader is:

```sh
cargo build -p ecraldr-x86_64-multiboot --target x86_64-unknown-none
```

For RISC-V, the selected loader is:

```sh
cargo build -p ecraldr-riscv64-none --target riscv64gc-unknown-none-elf
```

The kernel and the loader are still separate final artifacts, because they use
different relocation and linking requirements. The kernel is PIE-enabled and is
relocated into its final virtual layout. The loader is statically linked at the
firmware-visible physical address expected by the architecture entry path.

What changed is that the architecture-specific loader is now the Cargo binary
being built. There is no extra boot-module rlib that must be built separately
and then injected into another loader build with a manual `--extern`.

## Why the Change Was Necessary

### The loader is a first-class component

The loader is no longer just a tiny x86-64 bootstrap stub. It owns real behavior:

- embedding the stripped kernel;
- reserving and exposing the loader's own physical range;
- setting up the boot stack and early entry path;
- normalizing firmware-provided data into `BootArg`;
- selecting target-specific static link addresses;
- handing control to the kernel through a stable symbol.

Those responsibilities deserve a coherent namespace. Grouping the crates under
`ecraldr/` makes it obvious that they are one subsystem, not a set of unrelated
support crates.

### Architecture differences should be explicit

x86-64 Multiboot and RISC-V direct boot do not start from the same contract.
They differ in firmware interface, initial register contents, assembly setup,
link address, and platform argument source.

Putting those entry paths in separate binary crates keeps the differences
visible while preserving the shared parts:

- both depend on `ecraldr` for kernel embedding and common loader state;
- both depend on `ecraldr_base` for the kernel-entry ABI;
- both use `ecraldr_build_rs` to materialize their static linker script.

This is a cleaner split than hiding architecture-specific entry code behind a
generic boot-module crate name.

### The ABI crate should not sound like a boot implementation

`ecraos_boot` was doing more and less than its name suggested. It was not the
bootloader, and it was not tied to only ecraOS-side code. It defined the shared
contract between the loader and kernel.

`ecraldr_base` is a more precise name:

- `ecraldr` ties it to the loader subsystem;
- `base` describes the common ABI layer used by all loader binaries;
- the name leaves room for more target-specific loaders without implying that
  this crate is itself a platform boot implementation.

### The build graph should remain ordinary Cargo where possible

Earlier designs used manual artifact wiring for platform or boot components.
That style is fragile in Rust because metadata from separately compiled crate
graphs can leak across `--extern` boundaries. The previous `explat` analysis
already showed how this can produce confusing crate-identity failures once rich
Rust types cross the boundary.

The loader still needs two build phases because the kernel payload must exist
before the loader can embed it. But the second phase now builds one real loader
binary package through Cargo. Its platform entry code, common loader code, base
ABI crate, macro crate, and build helper are all normal dependencies of that
binary.

That keeps the unavoidable split at the artifact boundary:

```text
stripped kernel binary -> embedded loader payload
```

It avoids inventing another Rust metadata boundary inside the loader itself.

## Result

The refactor makes the loader side easier to inspect and harder to misuse:

- the loader subsystem has one directory, `ecraldr/`;
- the shared loader code is separated from target-specific entry binaries;
- the kernel-entry ABI has a neutral base crate name;
- architecture-specific loaders keep their descriptive hyphenated package names;
- `test.sh` builds the same target-specific loader artifact that QEMU runs;
- the build-helper crate is available where needed without becoming a normal
  workspace target.

The practical effect is a cleaner model for continuing x86-64 and RISC-V work.
New loader targets can be added as sibling `ecraldr-*` binaries while reusing
the same kernel embedding, boot argument, and kernel-entry machinery.
