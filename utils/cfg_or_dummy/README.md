# `cfg_or_dummy`

`cfg_or_dummy` provides the `#[cfg_or_dummy]` attribute macro. It adds a configuration-dependent
dummy definition for a free function while preserving the function's interface.

## What it generates

Apply the macro to a function with a `cfg` condition:

```rust
use cfg_or_dummy::cfg_or_dummy;

#[cfg_or_dummy(target_arch = "x86_64")]
fn f() {
    // The implementation for x86-64.
}
```

The macro emits two conditional definitions:

- it emits the original function and its implementation, with the original condition;
- it emits a dummy function with the same signature, visibility, attributes, generics, ABI, safety,
  and return type, with the negated condition.

The dummy keeps the function's name and interface available on configurations that do not provide
the implementation. Its body is `unreachable!()`, with no actual operation.

The condition uses the same metadata syntax as `#[cfg(...)]`, including compound conditions such
as `any(target_arch = "aarch64", target_arch = "riscv64")`.

## Main use case: architecture-specific documentation

The macro is mainly useful when an architecture-specific module must also be included in rustdoc:

```rust,no_run
#[cfg(any(target_arch = "x86_64", doc))]
pub mod x86_64_cpu {
    use cfg_or_dummy::cfg_or_dummy;

    #[cfg_or_dummy(target_arch = "x86_64")]
    pub fn pause() {
        // SAFETY: `pause` has no memory or stack effects exposed to Rust.
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(target_arch = "x86_64")]
x86_64_cpu::pause();
```

During a documentation build for another architecture, `doc` keeps the module enabled. The
function body may still contain architecture-specific imports, types, or instructions. Leaving the
body unconditionally enabled can therefore fail the documentation build. Marking the function with
`#[cfg(target_arch = "x86_64")]` avoids that error, but also removes the function from the
documentation.

`cfg_or_dummy` solves both parts of this problem. It keeps the function visible through the dummy
definition, while the architecture-specific implementation is compiled only when its condition
matches. The generated definitions carry `doc(cfg(condition))` so rustdoc records the condition
for the real implementation.

## Usage

Add the crate as a dependency:

```toml
[dependencies]
cfg_or_dummy = "0.1"
```

The attribute accepts one valid `cfg` metadata item and applies to free functions.
