# A Bad Error and the Drawbacks of `explat`

Date: 2026-06-13

## Summary

Commit `6bd5303cf3b83c292db66e4e348165097cfce504` fails to build with:

```text
error[E0463]: can't find crate for `explat`
error[E0463]: can't find crate for `expt`
error[E0463]: can't find crate for `exbuddy`
```

The previous commit, `8e320e70f51a86720c68b6dd58202b33a92b2aca`,
builds and boots with the same command:

```sh
cargo clean && QEMU_EXTRA_ARGS='-m 16G' PROFILE=debug ./test.sh
```

The failure is not a normal dependency-version problem. It is caused by the
current `explat_impl` linking model: one Cargo invocation builds
`explat-x86_64`, then another Cargo invocation builds `ecraos` while manually
injecting the first artifact with `--extern explat_impl=...`.

This creates multiple Rust crate identities for crates with the same package
name and version. Commit `6bd5303` makes that latent problem visible by exposing
`expt` types through the public `explat`/`explat_impl` interface.

## What Changed

The failing commit changed only the `explat` memory interface and its x86_64
implementation.

Important additions:

```rust
// explat/explat/src/lib.rs
pub mod reexport {
    pub mod expt {
        pub use expt::*;
    }
}
```

```rust
// explat/explat/src/mem.rs
use expt::opaque::OpaquePageTableType;
use memory_addr::VirtAddr;

pub trait MemIf {
    fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr>;
}
```

```rust
// explat/explat-x86_64/src/mem.rs
use explat::reexport::expt::{
    arch::x86_64::{X86Level4PageTableMeta, X86Level5PageTableMeta},
    opaque::OpaquePageTableType,
    pte::x86_64::X64PTE,
};
```

Before this commit, `explat` already depended on `expt`, and `ecraos` also
depended on `expt`. That alone was not enough to fail. The new failure appears
because `expt` becomes part of the public metadata crossing the manually
injected `--extern explat_impl=...` boundary.

## Build Model

`test.sh` builds the platform and kernel in separate Cargo invocations:

```sh
RUSTFLAGS="-C relocation-model=pie" \
    cargo build -p explat-x86_64 --target x86_64-unknown-none

RUSTFLAGS="-C relocation-model=pie \
    -C link-arg=-Tecraos/link.ld \
    --cfg building_ecraos \
    --extern explat_impl=$PLAT" \
    cargo build -p ecraos --target x86_64-unknown-none
```

This means `ecraos` sees two sources of platform-related metadata:

1. Cargo's normal dependency graph for `ecraos`, including `explat`, `expt`,
   `exbuddy`, `memory_addr`, etc.
2. The manually injected `libexplat_x86_64.rlib`, which was produced by an
   earlier and separate Cargo invocation.

Those two graphs are not guaranteed to contain the same Rust crate identities.

## Crate Identity

Rust does not identify a type only by `crate name + package version + type
name`. It identifies it by the specific compiled crate instance. Cargo and
rustc use metadata and disambiguators derived from the compilation unit.

A compilation unit includes factors such as:

- target triple;
- profile;
- feature set;
- `RUSTFLAGS`;
- `--cfg` values;
- `--extern` values;
- upstream dependency identities;
- selected package graph.

Therefore, two artifacts can both be built from `expt v0.1.0` source and still
be different Rust crates from rustc's point of view.

If one side has:

```text
expt-1655ecf020386599
```

and another side has:

```text
expt-1f62f85c1ebbd320
```

then `expt::opaque::OpaquePageTableType` from one side is not the same type as
`expt::opaque::OpaquePageTableType` from the other side.

## Evidence

Verbose builds show that the first stage and second stage produce different
crate identities.

First stage:

```text
RUSTFLAGS='-C relocation-model=pie' cargo build -p explat-x86_64 ...

expt:
  -C metadata=2b7492c48b09d04d
  -C extra-filename=-1655ecf020386599

explat:
  -C metadata=2e1d5dae85d55dd6
  -C extra-filename=-da289ee822b0df5b
```

Second stage:

```text
RUSTFLAGS='-C relocation-model=pie -C link-arg=-Tecraos/link.ld \
    --cfg building_ecraos --extern explat_impl=...' \
    cargo build -p ecraos ...

expt:
  -C metadata=9f9aa439e3c1a2ea
  -C extra-filename=-1f62f85c1ebbd320

explat:
  -C metadata=4d76d4626579a6bf
  -C extra-filename=-e35b90f4f1159028
```

The second-stage `RUSTFLAGS` are applied to all target dependencies, not only to
the final `ecraos` crate. For example, `memory_addr` is compiled with:

```text
-C relocation-model=pie
-C link-arg=-Tecraos/link.ld
--cfg building_ecraos
--extern explat_impl=...
```

Thus the second stage intentionally creates a different set of compilation
units.

There is another subtle source of difference. Even with identical
`RUSTFLAGS='-C relocation-model=pie'`, `cargo build -p explat-x86_64` and
`cargo build -p ecraos` do not necessarily reuse every dependency artifact.
The selected package graph and feature resolution can differ. In this project,
`explat-x86_64` directly depends on `x86_64` and enables features such as
`instructions` and `nightly`, while `ecraos` mostly receives `x86_64` through
`page_table_entry -> expt`. That can change the identity of upstream
dependencies, which then changes the identity of `page_table_entry`, `expt`,
and crates depending on them.

## Why the Error Appears as E0463

The immediate error is misleading:

```text
error[E0463]: can't find crate for `explat`
```

Cargo does pass an `--extern explat=...` argument to rustc during the second
stage. The problem is that rustc also loads metadata from the manually injected
`explat_impl` rlib. That metadata refers to the first-stage `explat` and `expt`
crate identities, while the current rustc invocation has the second-stage
identities available.

From rustc's perspective, the exact crate instance required by
`explat_impl` is not available. The name `explat` exists, but not with the
identity required by that metadata.

## Why `memory_addr` Did Not Expose the Problem Earlier

`memory_addr` was already shared by several crates and re-exported through
`explat`. The old commit still built.

This does not mean `memory_addr` had no duplicate identities. It means the old
public interface did not force rustc to reconcile those identities in a way that
failed. The `expt` change is different because it introduces a local, complex,
generic page-table type into the public interface crossing the
`explat_impl` boundary.

The important condition is not simply "two crates both depend on `expt`".
The important condition is "a type from `expt` appears in public metadata that
crosses from the separately built platform artifact into the kernel build".

## Why This Is an `explat` Design Problem

The current `explat` model asks Rust to do something that is fragile:

1. Compile platform implementation code in one Cargo graph.
2. Compile kernel code in another Cargo graph.
3. Inject the first artifact into the second with a raw `--extern`.
4. Expect Rust-level public types to remain identical across the boundary.

That can work only while the boundary is extremely narrow and avoids exposing
nontrivial Rust types from shared crates. Once the platform API grows and starts
returning page-table types, allocator types, generic wrappers, trait-related
types, or other kernel-side abstractions, crate identity mismatches become
likely.

This is why the problem is architectural, not just a bad import path.

## Local Mitigation

A narrow fix is to stop exposing `expt` through `explat`.

Instead of:

```rust
fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr>;
```

`explat` can return only platform facts:

```text
page size
virtual-address bits
LA48 or LA57 support
current address-space mode
```

Then `ecraos` can construct its own `expt::OpaquePageTableType` inside the
kernel's own Cargo graph. That avoids passing `expt` types through
`explat_impl`.

This would fix the immediate build failure, but it does not remove the
underlying fragility.

## Why a Thorough Fix May Require Removing `explat`

The deeper issue is that `explat` currently combines two roles:

1. a Rust API crate defining platform-kernel interfaces;
2. a separately compiled platform implementation artifact injected with
   `--extern`.

Those roles conflict when the interface needs to expose normal Rust types.

A thorough fix may require removing or radically simplifying `explat` so that
platform code and kernel code are compiled in one coherent Cargo graph. Possible
directions include:

- make platform implementations ordinary Cargo dependencies selected by
  features;
- compile platform modules directly as part of `ecraos`;
- use workspace features to choose the active platform;
- keep only a very thin, data-only ABI boundary if separate artifacts are still
  required;
- avoid exposing Rust generic/library types across separately built rlib
  boundaries.

Keeping the current `explat_impl` model means future platform interfaces will
need to be artificially constrained. As soon as VMM, page tables, CPU state,
per-CPU data, interrupt controllers, device abstractions, or allocators need to
cross the boundary, the same class of error can reappear.

Therefore, the build failure in `6bd5303` is not just a bad commit. It exposes a
fundamental drawback of the current `explat` architecture: it makes Rust crate
identity part of an implicit ABI between separately compiled graphs.
