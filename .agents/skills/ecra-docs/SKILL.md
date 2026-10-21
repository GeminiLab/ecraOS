---
name: ecra-docs
description: Use when adding, changing, or reviewing ecraOS doc comments, standalone documentation, instructions, or examples, and when code changes affect documented behavior.
---

# ecraOS documentation

## Rust comments

- Document every item, including private types, aliases, functions, and modules.
- Document modules with inner doc comments (`//!`) in the module's source file.
  This is an ecraOS convention. Rust also permits outer doc comments on `mod`
  declarations, but ecraOS does not use that form.
- Start every doc comment with a one-line summary, then a blank line, then a
  detailed description. If you find the item is simple, the blank line and the
  detailed description can be omitted.
- Use a third-person singular present-tense verb phrase for function summaries,
  such as "Initializes the VMM layout ...".
- Use noun phrases for type summaries, such as "The VMM layout." or "A memory region.".
- Use noun phrases describing contents or roles for module summaries, such as
  "Virtual memory management.".
- Start doc-comment headings at level 1 (`# Heading`), with lower levels for subheadings.
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

Explain contracts, invariants, and reasons that help the reader use or maintain the
code. Document safety requirements where applicable, and update comments when
behavior changes.

## Standalone documents and examples

- Match the existing language and terminology of the document.
- Keep descriptions consistent with current code, scripts, and configuration.
  Verify commands and defaults against their implementation before documenting them.
- Explain significant design choices and limitations at the depth the task needs.
  A small change does not require a separate design document.
- Maintain one authoritative location for each rule or procedure and link to it
  from other documents. Resolve relative links from the containing file.
- Use `ecra-verify` to check links, examples, and claims before reporting completion.
