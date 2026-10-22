# Multi-Architecture Rustdoc Configuration

This document describes how ecraOS builds API documentation for multiple processor
architectures while preserving ordinary target-specific builds. It records the
boundary between Cargo dependency selection and rustdoc configuration, explains the
failure modes of globally propagated cfg values, and defines the current
`ecraos_doc` convention.

The design discussion in [`page_table_multiarch` PR #47](https://github.com/arceos-org/page_table_multiarch/pull/47)
is useful background. The local implementation is visible in the
[`page_table_entry` manifest](../../page_table_entry/Cargo.toml), its
[architecture module declarations](../../page_table_entry/src/arch/mod.rs), and
[`expt`'s opaque page-table types](../../expt/src/opaque.rs).

## Goals

A multi-architecture documentation build should:

- include APIs for all supported architectures and the dependencies those APIs need;
- mark target-specific APIs with their target requirements;
- preserve normal target-specific builds; and
- work locally and on docs.rs.

A plain `cargo doc` should also document the active target successfully, even if it
does not include APIs for other architectures.

## Cargo, rustc, and rustdoc have different scopes

| Component | Responsibility | Relevant configuration |
| --- | --- | --- |
| Cargo | Reads manifests, resolves dependencies, and plans compiler invocations | Target predicates and cfg values supplied to the Cargo build |
| rustc | Compiles each crate and applies source-level conditional compilation | `#[cfg(...)]` and `cfg_attr(...)` |
| rustdoc | Documents the crate currently being processed | Built-in `doc` and flags passed to the rustdoc invocation |

Cargo selects dependencies before invoking rustc or rustdoc. A cfg that is only
available to rustdoc cannot influence Cargo's earlier dependency-selection step.
This ordering is the source of the mismatch described below.

## Why `cfg(doc)` cannot select target-specific dependencies

`doc` is a built-in cfg that rustdoc enables while processing the crate being
documented. It is useful for source-level documentation behavior, but Cargo does
not receive it by default when selecting dependencies.

For example, this source condition exposes an AArch64 module during documentation:

```rust
#[cfg(any(target_arch = "aarch64", doc))]
pub mod aarch64 {
    use aarch64_cpu::*;
}
```

But the matching manifest condition does not work on a non-AArch64 host:

```toml
[target.'cfg(any(target_arch = "aarch64", doc))'.dependencies]
aarch64-cpu = "11.0"
```

Cargo does not know that `doc` is enabled when it evaluates the target dependency.
It therefore omits `aarch64-cpu` on an x86-64 host, while rustdoc later enables the
module. Compilation then fails because the module imports a crate that is not in
the dependency graph.

This limitation is tracked in [Cargo issue #11570](https://github.com/rust-lang/cargo/issues/11570)
and described in Cargo's [target configuration](https://doc.rust-lang.org/cargo/reference/config.html#target)
and [platform-specific dependency](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies)
documentation. Use `doc` for source-level choices that concern the crate rustdoc is
currently processing, not for selecting manifest dependencies.

## Why globally passing `--cfg doc` is unsafe here

Passing `--cfg doc` through `RUSTFLAGS` can make Cargo evaluate a manifest condition
that mentions `doc`. However, `RUSTFLAGS` also affects dependency compiler
invocations. It would enable `doc` in every crate in the dependency graph, not only
the crate being documented.

For example, `quote` has a `cfg(doc)` branch that substitutes a placeholder `quote!`
macro for its own rustdoc examples. If the dependency is compiled with globally
propagated `doc`, downstream code may receive the placeholder macro, whose output
is not valid application code.

Cargo currently has no manifest setting that applies an arbitrary cfg to only the
root package while excluding its dependencies. In ecraOS's build, globally passing
`--cfg doc` therefore changes too much state to serve as a reliable solution.

## Why `docsrs` is not the local project cfg

`docsrs` is a custom cfg used by docs.rs. Its [build documentation](https://docs.rs/about/builds)
states that docs.rs applies it to the final rustdoc invocation for the crate being
documented, not to dependency compilation. This default scope is narrower than local
`RUSTFLAGS="--cfg docsrs"`, which propagates the cfg to dependency builds as well.

The docs.rs [`rustc-args` metadata option](https://docs.rs/about/metadata) adds compiler
flags to the package build, so Cargo can evaluate dependency conditions that use
the cfg. ecraOS uses this mechanism with its own `ecraos_doc` cfg for dependency
selection instead of relying on `docsrs` for that role. The metadata setting is
separate from docs.rs' default `docsrs`
cfg behavior. Since the cfg can also reach dependency compiler invocations,
`ecraos_doc` reduces collision risk but does not isolate dependencies from cfg
propagation.

Rustdoc's automatic cfg inference (now provided by `doc_cfg`, formerly called
`doc_auto_cfg`) is not a substitute for explicit target documentation attributes
in this setup. Automatic inference does not remove the custom `docsrs` cfg from
rendered conditions. Use explicit
`cfg_attr(ecraos_doc, doc(cfg(...)))` attributes to describe the API's actual target
requirement.

## The `ecraos_doc` convention

Cargo must receive a cfg to select architecture-specific dependencies for a
multi-architecture documentation build. That cfg is also propagated into dependency
compilations. ecraOS uses the crate-specific name `ecraos_doc` to make accidental
collisions with cfgs used by external dependencies unlikely.

### Register the cfg in each crate

Every crate whose Rust source references `ecraos_doc` should declare it in its own
`build.rs`:

```rust
fn main() {
    println!("cargo::rustc-check-cfg=cfg(ecraos_doc)");
}
```

This registers the cfg name for checking and avoids `unexpected_cfgs` warnings. It
does not enable the cfg; build flags or package metadata do that.

### Select dependencies and expose their APIs together

Add `ecraos_doc` to target-specific dependency predicates where those dependencies
are needed for multi-architecture documentation:

```toml
[target.'cfg(any(target_arch = "aarch64", ecraos_doc))'.dependencies]
aarch64-cpu = "11.0"
```

Use a corresponding source condition to expose the dependent module and label its
target requirement:

```rust
#[cfg(any(target_arch = "aarch64", ecraos_doc))]
#[cfg_attr(ecraos_doc, doc(cfg(target_arch = "aarch64")))]
pub mod aarch64 {
    use aarch64_cpu::*;
}
```

The real target predicate preserves the ordinary AArch64 build. `ecraos_doc` makes
the dependency and module available together in the multi-architecture
documentation build.

The crate that emits `doc(cfg(...))` attributes must enable the `doc_cfg` language
feature. A package that only needs it for multi-architecture documentation can use
`#![cfg_attr(ecraos_doc, feature(doc_cfg))]` at crate level. The appropriate feature
gate depends on how that package uses `doc_cfg` elsewhere.

### Configure docs.rs and local builds

For every package that needs `ecraos_doc` to select conditional dependencies on
docs.rs, add this metadata to that package's own `Cargo.toml`:

```toml
[package.metadata.docs.rs]
rustc-args = ["--cfg", "ecraos_doc"]
```

The `rustc-args` setting lets the docs.rs build evaluate the package's conditional
dependencies with `ecraos_doc` enabled. Put the metadata in each relevant package;
a workspace-root setting does not configure every member package.

For a local multi-architecture build, pass the cfg to both Cargo's rustc
invocations and the final rustdoc invocation:

```sh
RUSTFLAGS="--cfg ecraos_doc" RUSTDOCFLAGS="--cfg ecraos_doc" \
    cargo doc -p page_table_entry --all-features --no-deps
```

Use the same approach for another package that declares `ecraos_doc`-gated
conditional dependencies. Leave both environment variables unset for an ordinary
target-specific documentation build.

## Keep dependency type definitions stable

A dependency may compile successfully with `ecraos_doc` enabled while exposing a
different type definition. That can break code in another crate that relies on the
normal target-specific type shape.

For example, `expt` exposes a dual page-table root for AArch64. When `expt` itself is
documented, the extra variant should appear in the reference. But when `expt` is a
dependency of `exarch` during a multi-architecture build, globally propagated
`ecraos_doc` also enables that variant in the x86-64 build. Code that relies on
`OpaquePageTableRoot` having only the `Single` variant then fails:

```rust
let OpaquePageTableRoot::Single(page_table_root) = page_table_root;
```

The pattern is irrefutable when the enum has only one variant and refutable when
`Dual` is also present, producing E0005. Hiding the entire function body behind a
cfg and adding an `unreachable!()` branch would silence the error but would leave
placeholder behavior in the documentation build.

Use `doc` for the type-shape decision that applies only while documenting `expt`
itself:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaquePageTableRoot {
    Single(PhysAddr),
    #[cfg(any(test, doc, target_arch = "aarch64"))]
    #[cfg_attr(doc, doc(cfg(target_arch = "aarch64")))]
    Dual(crate::pt::DualPageTableRoot),
}
```

When rustdoc documents `expt`, `doc` and `ecraos_doc` are both enabled, so the
multi-architecture reference includes `Dual`. When another crate compiles `expt` as
a dependency, `doc` is not enabled for that dependency, so its type definition
remains unchanged.

The rule is to use `ecraos_doc` when code must appear together with conditional
dependencies, and `doc` when a source-level change should apply only to the crate
rustdoc is currently documenting.

## Maintenance checklist

When adding a target-specific API or dependency:

1. Add `ecraos_doc` to the package's conditional dependency predicate where needed.
2. Use a matching source cfg for APIs that need those dependencies, and add
   `doc(cfg(...))` metadata for their target requirements.
3. Use `doc` for source conditions that should affect only the crate being
   documented, such as a type shape that must not change in dependencies.
4. Register `ecraos_doc` in that package's `build.rs` if its source uses the cfg.
5. Add docs.rs `rustc-args` metadata to that package when docs.rs must select its
   conditional dependencies.
6. Check whether enabling the cfg in dependencies changes public type definitions
   or other behavior relied on by downstream crates.
7. For local multi-architecture docs, pass the cfg through both `RUSTFLAGS` and
   `RUSTDOCFLAGS`.

This convention reflects the current Cargo and rustdoc behavior. It keeps
multi-architecture dependency selection explicit, while using the built-in `doc`
cfg for changes that must remain local to the crate being documented.
