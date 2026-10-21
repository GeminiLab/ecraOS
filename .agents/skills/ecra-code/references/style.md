# ecraOS Rust code style guide

## Item and import ordering

Order Rust source contents as follows, with a blank line between groups:

1. Doc comments.
2. Other outer attributes.
3. Extern crate definitions.
4. Uses from the standard library crates `core`, `alloc`, and `std`.
5. Uses from other crates, one statement per crate.
6. Uses from the current crate, merged into a single `use crate::...` statement.
7. Module definitions, public and private together.
8. Uses from self or submodules, one statement per submodule, starting with
   `use submodule_name::...;` or `use self::*`.
9. Other items.

If a use statement resolves macro visibility, place it immediately after the macro
definition instead. Merge uses from the same crate into a single use statement.

Doc comment content is defined by `ecra-docs`. Formatting and lint commands are
defined by `ecra-verify`.

## Prefer semantic types

ecraOS favors semantic types over raw representations. For example, use
`VirtAddr`, `PhysAddr`, `VirtAddrRange`, and `PhysAddrRange` for addresses and
address ranges, instead of `usize` or `core::ops::Range<usize>`, where appropriate.

Apply these rules:

- If a suitable semantic type is available, use it instead of the raw type. Use a
  raw type when no suitable semantic type is available or when it is inappropriate
  for the context, such as when interacting with assembly or code outside ecraOS.
- If a semantic type is appropriate but none is available:
  - When designing a crate or module, or refactoring one, propose a new semantic
    type to the user and follow their decision.
  - When fixing a bug or implementing a feature, keep the existing raw type.
