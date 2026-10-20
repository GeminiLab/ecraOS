# `look_at`

`look_at` provides the [`look_at!`] procedural macro. It exposes the items of one selected
implementation module, usually chosen per architecture, through a list of declarations written once
at the invocation site. Function declarations become forwarding wrappers. Type, constant, and `use`
declarations become re-exports or typed constants.

## Usage

Add the crate as a dependency and import the macro:

```toml
[dependencies]
look_at = "0.1"
```

Then use the macro to expose the items of the selected implementation module:

```rust
use look_at::look_at;

mod x86_64 {
    pub fn page_shift() -> u32 {
        12
    }

    pub const MAX_CPUS: usize = 64;
}

mod fallback {
    pub fn page_shift() -> u32 {
        12
    }

    pub const MAX_CPUS: usize = 1;
}

#[cfg(target_arch = "x86_64")]
use x86_64 as current;
#[cfg(not(target_arch = "x86_64"))]
use fallback as current;

look_at! {
    @current:
    pub fn page_shift() -> u32;
    pub const MAX_CPUS: usize;
}

assert_eq!(page_shift(), 12);
assert!(MAX_CPUS >= 1);
```

An invocation starts with `@`, a path to the implementation module, and `:`. Each following
declaration ends with `;` and resolves below that path. The invocation must contain at least one
declaration.

## Compared with conditional re-exports

The `#[cfg]` imports above still select the implementation module. Re-exporting from it directly,
for example with `pub use current::page_shift;`, would also give callers one set of names.
Declaring the interface with `look_at!` adds the following:

- Each declaration is checked against the selected module in the crate that declares the
  interface. Diverging function signatures, constant types, and type bounds are rejected even
  when no caller uses the item.
- Function wrappers and forwarding constants are new items, so they can be public while their
  targets stay crate-private. A `pub use` cannot re-export such targets.

In exchange, function wrappers are distinct functions, signatures are written once more, and some
declarations are checked only partially. The [`look_at!`] documentation describes the complete
comparison, including when a conditional re-export is the simpler choice.

## Declarations

| Declaration | Expansion |
| --- | --- |
| `fn name(...) -> T;` | A wrapper that calls `path::name` |
| `type Name;` | `use path::Name;` |
| `type Name: Bound;` | The re-export and a compile-time check that `path::Name: Bound` |
| `const NAME: T;` | `const NAME: T = path::NAME;` |
| `use tree;` | `use path::tree;` |

See the [`look_at!`] documentation for the rules of each declaration kind, the handling of
attributes, visibility, and doc comments, and the items that cannot be declared.
