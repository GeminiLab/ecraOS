---
name: ecra-docs
description: Use when adding, changing, or reviewing ecraOS doc comments, standalone documentation, instructions, or examples, and when code changes affect documented behavior.
---

# ecraOS documentation

## Documentation organization

- Document every Rust item, including private types, aliases, functions, and
  modules.
- Use a standalone crate-level `README.md` as the crate doc with
  `#![doc = include_str!("../README.md")]`.
- For public items, the purpose of documentation should be to help readers use
  the code. For private items (including `pub(crate)` items), the purpose of
  documentation should be to help maintainers maintain the code. The visibility
  of the item determines the level of detail the documentation should have.
- Use crate-level and module-level documentation to explain architecture, conceptual
  models, crate usage, and relationships between components.
- Use type-level documentation to explain abstractions, state models, ownership,
  lifecycle, and invariants for the type.
- Use function and method documentation to explain contracts and semantics for
  the item, and its relationship with closely related items.
- Keep documentation at each level focused on information most useful at that level.
  Do not force crate-wide design explanations into every API comment.
- Maintain one authoritative location for each shared rule, procedure, or design
  explanation. Reference that location from other documents rather than duplicating
  the same information.
- Include examples when they materially help readers understand how to use an API.
  Prefer examples that demonstrate realistic usage and important constraints.
  Verify that examples compile and demonstrate behavior consistent with the
  documented contracts. Use `ignore` and `no_run` only when really necessary.

## Rust comments

- Document modules with inner doc comments (`//!`) in the module's source file.
  This is an ecraOS convention. Rust also permits outer doc comments on `mod`
  declarations, but ecraOS does not use that form.
- Start every doc comment with a one-line summary, then a blank line, then a
  detailed description. If the item is simple, the blank line and detailed
  description can be omitted.
- The one-line summary should focus on the role and key functionality of the
  item, not its implementation details.
- Use a third-person singular present-tense verb phrase for function summaries,
  such as "Initializes the VMM layout ...".
- Use noun phrases for summaries of types and constants, such as "The VMM
  layout.", "A memory region.", "Size of the slab header region.".
- Use noun phrases or a sentence for enum variant summaries, such as "The object
  was released and its slab stays cached.", "A semantic local TLB invalidation.".
- Use verb phrases or noun phrases for impl block summaries, such as
  such as "Iterates over managed sections immutably.", "Operations on an opaque
  multi-level page table.".
- Use noun phrases describing contents or roles for module summaries, such as
  "Virtual memory management.".
- Start doc-comment headings at level 1 (`# Heading`), with lower levels for
  subheadings. However, for crate doc-comments in `README.md`, use level 1
  headings for title and start at level 2 (`## Heading`).
- Avoid semicolons in doc comments. Wrap comments at 100 characters.

For example:

```rust
/// Returns the number of available pages.
///
/// Counts pages that can be allocated without growing the pool.
pub fn available_pages(&self) -> usize {
    self.available
}
```

### API contracts and semantics

For every public API:

- Document the observable behavior, not merely what its name or implementation
  suggests. Describe its purpose, semantics, and constraints in enough detail
  that users can understand and use it without reading its implementation.

For every API, no matter public or private:

- Document relevant preconditions, postconditions, invariants, and guarantees.
  Distinguish requirements imposed on callers from guarantees provided by the API.
- Document parameter and return-value semantics, including ownership transfer,
  borrowing, lifetime constraints, state changes, and resource management where relevant.
- Document safety requirements, panic conditions wherever applicable, in the
  `# Safety` and `# Panics` sections.
- Document important errors in the `# Errors` sections. This rule applies only
  when the signature or the documents of the error type is not enough to describe
  the failure behavior.
- For traits, document the semantic contracts for implementors and users, including
  required invariants, behavioral guarantees, and expectations that cannot be
  expressed by the type system.
- For concurrent APIs, document relevant synchronization guarantees, memory-ordering
  requirements, atomicity, and permitted concurrency where applicable.
- Explain meaningful edge cases and limitations, including behavior for empty inputs,
  boundary values, invalid states, and resource exhaustion where relevant.
- Do not restate information that is already unambiguously expressed by the type
  signature unless additional explanation is useful.

## Technical accuracy

- Base technical claims on the actual implementation, tests, configuration, and
  established API contracts.
- Do not infer guarantees, constraints, or design intentions solely from names,
  comments, or apparent implementation patterns.
- Distinguish facts directly established by the code from interpretations of
  design intent or assumptions that require confirmation when writing documents
  or answering the user.
- Do not invent missing behavior or silently resolve ambiguities. Identify unclear
  semantics and ask the user when clarification is necessary.
- Ensure that documentation remains synchronized with the implementation, and do not
  describe implementation-dependent behavior as a stable API guarantee.

## Terminology

- Use established technical terms consistently and with their standard meanings.
- Introduce a project-specific term only when existing terminology does not
  accurately describe the concept. Define it on first use. Ask the user for
  permission to define it.
- Explain specialized terms and abbreviations when the intended audience may not
  know them.
- Preserve distinctions between concepts that have different technical meanings,
  even when they appear similar in the implementation.

## Standalone documents

- Create `README.md` for every crate. Create other standalone document files at
  the request of the user.
- Match the existing language and terminology of the document.
- Keep descriptions consistent with current code, scripts, and configuration.
  Verify commands and defaults against their implementation before documenting them.
- Explain significant design choices and limitations at the depth the task needs.
  A small change does not require a separate design document.
- Use `ecra-verify` to check links, examples, and claims before reporting completion.
