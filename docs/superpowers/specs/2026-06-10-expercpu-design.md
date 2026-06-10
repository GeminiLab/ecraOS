# expercpu Design

## Context

`expercpu` and `expercpu_macros` provide per-CPU data definitions and accessors
for ecraOS. The implementation will be based on `refs/percpu`, with changes for
the ecraOS memory model and feature policy.

The current `expercpu` crates are skeletons. `refs/percpu` provides the
reference implementation, including a runtime crate, a proc-macro crate, Linux
runtime tests, a test linker script, architecture-specific per-CPU register
access, and optional features that are not all wanted in ecraOS.

## Goals

- Implement `expercpu` and `expercpu_macros` according to `refs/percpu`.
- Remove the `sp-naive` feature and all code enabled only by that feature.
- Make the `non-zero-vma` behavior unconditional and remove the feature.
- Remove `init_in_place`.
- Change `init` so each CPU calls it with its own per-CPU area base virtual
  address.
- Avoid assuming that different CPUs' per-CPU areas are virtually contiguous.
- Gate remote CPU per-CPU access behind a new `remote-access` feature.
- Preserve Linux runtime tests, adapted to the new initialization and remote
  access model.

## Non-Goals

- Integrating `expercpu` into the ecraOS boot flow.
- Adding `.percpu` symbols to `ecraos/link.ld`.
- Implementing a platform-specific provider of remote per-CPU area bases in
  ecraOS itself.
- Reworking the reference implementation beyond the requested API and feature
  changes.

## Crate Structure

`expercpu/expercpu/src/lib.rs` will contain the full runtime implementation.
There will be no separate `imp.rs` module. It will:

- Export `expercpu_macros::def_percpu`.
- Use `memory_addr::VirtAddr` internally for public initialization and register
  APIs.
- Define `.percpu` linker symbols.
- Define per-CPU area sizing and layout helpers.
- Define `init`, `read_percpu_reg`, and `write_percpu_reg`.
- Define the x86_64 `SELF_PTR` per-CPU static used by generated x86_64 accessors.
- Define `__priv::NoPreemptGuard` when `preempt` is enabled.
- Define `PerCPUAreaIf` only when `remote-access` is enabled.

`expercpu` will not re-export `VirtAddr`.

`expercpu/expercpu_macros/src/lib.rs` and `arch.rs` will be adapted from
`refs/percpu/percpu_macros`. The naive implementation file and all `sp-naive`
configuration will be removed.

## Runtime API

The base runtime API is:

```rust
pub use expercpu_macros::def_percpu;

pub fn percpu_area_size() -> usize;
pub fn percpu_area_layout() -> core::alloc::Layout;
pub fn read_percpu_reg() -> memory_addr::VirtAddr;
pub unsafe fn write_percpu_reg(base: memory_addr::VirtAddr);
pub fn init(base: memory_addr::VirtAddr);
```

`init` has no `InitError` and returns `()`. It validates its input with explicit
assertions instead of returning an error. The base address must be non-null and
64-byte aligned.

`percpu_area_layout` returns the layout for one CPU's per-CPU area. It replaces
the reference implementation's `percpu_area_layout_expected(cpu_count)` because
`expercpu` no longer owns or assumes a contiguous allocation for all CPUs.

## Initialization

Each CPU calls:

```rust
expercpu::init(this_cpu_percpu_base);
```

`init` performs these steps:

1. Assert that `this_cpu_percpu_base` is non-null and 64-byte aligned.
2. Copy the initial `.percpu` image from `_percpu_start` into the supplied base.
3. Write the current CPU's architecture-specific per-CPU register to point to
   the supplied base.

The implementation does not store a global base address. It does not accept a
CPU count. It does not initialize other CPUs.

`read_percpu_reg` returns the current CPU's per-CPU area base as a `VirtAddr`.
`write_percpu_reg` accepts the current CPU's per-CPU area base as a `VirtAddr`.
The non-zero VMA adjustment from the reference implementation is always applied.

## Macro Expansion

`#[def_percpu]` generates the same basic wrapper structure as the reference
implementation:

- A hidden mutable static named `__PERCPU_<NAME>` in the `.percpu` section.
- A zero-sized wrapper type named `<NAME>_WRAPPER`.
- A public or private static named `<NAME>` with the original visibility.

The wrapper provides:

- `symbol_vma`
- `offset`
- `current_ptr`
- `current_ref_raw`
- `current_ref_mut_raw`
- `with_current`
- `reset_to_init`
- Primitive integer and bool fast paths:
  - `read_current_raw`
  - `write_current_raw`
  - `read_current`
  - `write_current`

The macro expansion will refer to the runtime crate as `expercpu`.

The `non-zero-vma` behavior is unconditional. Offsets are always calculated as:

```rust
symbol_vma - _percpu_load_start_vma
```

## Remote Access

Remote access is unavailable by default.

When `remote-access` is enabled, `expercpu` defines:

```rust
#[crate_interface::def_interface(gen_caller)]
pub trait PerCPUAreaIf {
    fn percpu_area_base_for(cpu_id: usize) -> memory_addr::VirtAddr;
}
```

Generated wrappers also gain:

```rust
pub unsafe fn remote_ptr(&self, cpu_id: usize) -> *const T;
pub unsafe fn remote_ref_raw(&self, cpu_id: usize) -> &T;
pub unsafe fn remote_ref_mut_raw(&self, cpu_id: usize) -> &mut T;
```

These methods call the generated `percpu_area_base_for(cpu_id)` caller and add
the per-CPU variable offset. They do not calculate remote bases from a global
base or a contiguous stride.

The caller is responsible for implementing `PerCPUAreaIf`, ensuring the CPU ID
is valid, ensuring the returned virtual address is accessible in the current
address space, and preventing data races.

## Features

`expercpu` features:

```toml
[features]
default = []
remote-access = ["dep:crate_interface", "expercpu_macros/remote-access"]
preempt = ["expercpu_macros/preempt", "dep:kernel_guard"]
arm-el2 = ["expercpu_macros/arm-el2"]
```

`expercpu_macros` features:

```toml
[features]
default = []
remote-access = []
preempt = []
arm-el2 = []
```

Removed features:

- `sp-naive`
- `non-zero-vma`

## Dependencies

`expercpu` dependencies:

- `cfg-if.workspace = true`
- `expercpu_macros = { path = "../expercpu_macros" }`
- `memory_addr.workspace = true`
- `crate_interface = { version = "0.3", optional = true }`
- `kernel_guard = { version = "0.2", optional = true }`
- `x86 = "0.52"` on `target_arch = "x86_64"`

`expercpu_macros` dependencies:

- `proc-macro2 = "1.0"`
- `quote = "1.0"`
- `syn = { version = "2.0", features = ["full"] }`

When `preempt` is enabled, `expercpu::__priv::NoPreemptGuard` re-exports
`kernel_guard::NoPreempt` for generated accessors, matching the reference
implementation.

## Linux Runtime Tests

Linux runtime tests will be retained and adapted from `refs/percpu`.

The test support will include:

- `build.rs`, linking `test_percpu.x` for Linux tests.
- `test_percpu.x`, defining the `.percpu` section and linker symbols needed by
  generated per-CPU statics.

Default-feature tests will cover local access:

- Allocate one per-CPU area.
- Call `expercpu::init(VirtAddr::from_usize(base))`.
- Assert that `read_percpu_reg()` returns the supplied base.
- Verify `current_ptr`, `current_ref_raw`, `with_current`, primitive
  `read_current` and `write_current`, and `reset_to_init`.

`remote-access` tests will cover remote access:

- Allocate multiple per-CPU areas without depending on contiguous layout.
- Initialize or seed each area with the `.percpu` initial image.
- Implement `PerCPUAreaIf` in the test crate so `cpu_id` maps to the allocated
  base for that CPU.
- Verify `remote_ptr`, `remote_ref_raw`, and `remote_ref_mut_raw` use the
  interface-provided base.

There will be no `sp-naive` tests and no `init_in_place` tests.

## Verification

Implementation verification will run:

```sh
cargo fmt
cargo check -p expercpu --target x86_64-unknown-none
cargo check -p expercpu --target x86_64-unknown-none --features remote-access
cargo check -p expercpu_macros
cargo test -p expercpu --target x86_64-unknown-linux-gnu
cargo test -p expercpu --target x86_64-unknown-linux-gnu --features remote-access
```

If existing workspace changes prevent a full workspace check, the implementation
will report that explicitly and keep verification scoped to `expercpu` and
`expercpu_macros`.
