# From `explat` to `exarch`

Date: 2026-06-14

## Context

ecraOS previously used `explat` as a platform abstraction layer. The design was
inspired by ArceOS `axplat`, but it intentionally changed the build model: the
platform implementation was built first, then injected into the kernel build
with `--extern explat_impl=<path>`.

The migration in `f0cfc97` removes `explat` and introduces `exarch`. The kernel
now depends on `exarch` as a normal workspace crate. The architecture-specific
code and the portable kernel code are compiled in the same Cargo graph.

## What `explat` Got Right

`explat` had real advantages:

- It made the platform boundary explicit. Kernel code called `explat::mem`,
  `explat::init`, `explat::power`, and `explat::debug_console` instead of
  reaching directly into architecture-specific files.
- It encouraged a small set of well-known platform contracts. The memory map,
  virtual-address-space mode, early initialization, debug console, and poweroff
  behavior were all grouped as platform responsibilities.
- It was conceptually close to ArceOS `axplat`, so it matched a proven pattern:
  keep the portable kernel mostly independent from board/platform details.
- It made platform replacement look direct at the API level. A different
  platform implementation could provide the same traits and leave most kernel
  code untouched.

Those properties are still valuable. `exarch` keeps the idea of explicit
architecture-kernel contracts, but changes where and how the implementation is
compiled.

## Why `explat` Became a Problem

The failing `explat` model used two Cargo invocations for the kernel side:

```sh
RUSTFLAGS="-C relocation-model=pie" \
    cargo build -p explat-x86_64 --target x86_64-unknown-none

RUSTFLAGS="-C relocation-model=pie \
    -C link-arg=-Tecraos/link.ld \
    --cfg building_ecraos \
    --extern explat_impl=$PLAT" \
    cargo build -p ecraos --target x86_64-unknown-none
```

That created two Rust crate graphs:

1. the graph used to build `explat-x86_64`;
2. the graph used to build `ecraos`.

Those graphs could contain crates with the same package name and source code,
but different Rust crate identities. Rust type identity is based on the
compiled crate instance, not just the package name and version. Different
`RUSTFLAGS`, `--cfg` values, feature sets, selected root packages, or upstream
dependency identities can all produce different crate instances.

This was documented in
[`2026-06-13-a-bad-error-and-the-drawbacks-of-explat.md`](2026-06-13-a-bad-error-and-the-drawbacks-of-explat.md).
The immediate build failure appeared after `expt` types crossed the
`explat_impl` boundary:

```text
error[E0463]: can't find crate for `explat`
error[E0463]: can't find crate for `expt`
error[E0463]: can't find crate for `exbuddy`
```

The error was misleading. Cargo did pass crates named `explat`, `expt`, and
`exbuddy` to rustc. The problem was that the `explat_impl` rlib metadata
expected the crate identities produced by the first Cargo graph, while the
current kernel build had identities from the second graph.

This means `explat` could only stay reliable if the boundary remained very
narrow and avoided nontrivial Rust types. Once the platform interface needed to
return page-table descriptions, opaque page-table types, allocator-related
types, or other generic kernel-side abstractions, the split graph became a
structural hazard.

## Why Not Just Use `axplat`

ArceOS `axplat` avoids the exact `--extern` metadata problem by making platform
crates ordinary dependencies of the final binary. That is simpler for Rust's
crate identity model.

However, it has an important flexibility cost: the final binary project must
explicitly include the platform crates it may use. When there are multiple
platform crates for one architecture, the binary-level dependency list and
feature selection become part of platform selection. That is acceptable for the
ArceOS design, but it is not a good fit for ecraOS's goals.

ecraOS wants strong flexibility, and does not currently need multiple platform
crates for a single architecture. One coherent architecture implementation per
architecture is a viable and simpler model. For x86-64, the architecture code
can live under one `exarch` implementation instead of being split into several
board-like platform crates.

`explat` tried to avoid the axplat-style requirement that the final binary
explicitly include all possible platform crates by injecting the selected
platform artifact with `--extern`. That solved one kind of inflexibility, but
created a worse problem: Rust metadata from separately compiled crate graphs
became part of an implicit ABI.

The current direction rejects both extremes:

- not axplat's final-binary list of platform crates;
- not explat's separately compiled `--extern explat_impl` boundary.

Instead, ecraOS uses `exarch`: one architecture support crate compiled in the
same graph as the kernel.

## How `exarch` Differs

`exarch` is an architecture support crate, not a separately injected platform
implementation. `ecraos` depends on it normally:

```toml
exarch = { path = "../exarch" }
```

The current `test.sh` builds the kernel in one step:

```sh
RUSTFLAGS="-C relocation-model=pie -C link-arg=-Tecraos/link.ld" \
    cargo build -p ecraos --target x86_64-unknown-none
```

This means `ecraos`, `exarch`, `expt`, `memory_addr`, and related crates are
resolved and compiled as one Cargo graph. `exarch` can expose
`expt::opaque::OpaquePageTableType` without crossing a separately compiled rlib
boundary.

Architecturally, `exarch` keeps the useful part of `explat`: kernel code still
asks architecture support code for memory regions, virtual-address-space modes,
page-table type selection, debug console output, early initialization, and
poweroff. The difference is that those contracts now live inside one coherent
Rust compilation graph.

This is a better fit for the current ecraOS design:

- architecture support can use rich Rust types directly;
- the kernel does not need a prebuilt platform artifact;
- the build script is simpler;
- one architecture can use one code path instead of a collection of platform
  crates selected by the final binary.

## Why `crate_interface` Remains

`exarch` still uses `crate_interface` even though the implementation now lives
inside a single crate. This is intentional for now.

The interfaces in `exarch::debug_console`, `exarch::init`, `exarch::mem`, and
`exarch::power` document what the architecture-specific modules must provide.
They also make call sites read like stable contracts:

```rust
exarch::mem::raw_mem_regions(...)
exarch::mem::set_virt_addr_space_mode(...)
exarch::power::poweroff()
```

The practical benefits are:

- the required architecture hooks are visible in one place;
- portable kernel code does not depend on the concrete x86-64 module layout;
- adding another architecture later has a checklist of required functions;
- implementation modules stay decoupled from the exact call sites.

This use of `crate_interface` is much less risky than the old `explat_impl`
model, because it no longer crosses separately compiled Cargo graphs. It is an
internal organization tool, not an rlib ABI boundary.

That said, `crate_interface` may still be heavier than necessary. Future work
could replace it with a lighter local mechanism, such as explicit module
functions, a small local macro, or architecture-specific modules selected by
`cfg(target_arch)`. The current choice is kept because it clearly expresses the
contracts during the transition from `explat` to `exarch`.

## Result

The migration to `exarch` keeps explicit architecture-kernel contracts while
removing the fragile Rust metadata boundary created by `--extern explat_impl`.
It also avoids the axplat-style requirement that final binary crates enumerate
all possible platform crates.

For ecraOS's current goals, a single architecture support crate per
architecture is the more flexible and more robust design.
