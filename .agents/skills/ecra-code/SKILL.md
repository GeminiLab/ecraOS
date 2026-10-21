---
name: ecra-code
description: Use when writing, modifying, or reviewing code in ecraOS, including bug fixes, refactoring, and code style changes. Pure documentation work does not require this skill.
---

# ecraOS code work

## Before changing code

- Read the affected implementation and its callers. Identify the behavior or
  interface that the change must preserve or alter.
- For Rust work, read [the style rules](references/style.md).
- For behavior changes, use `ecra-verify` to choose meaningful checks before implementation.
- Use `ecra-docs` when adding items or changing documentation or documented behavior.

## Implementation and review

- Follow existing abstractions. Introduce new abstractions when they clarify the
  current change, and explain significant tradeoffs.
- Keep unrelated cleanup out of the diff. Fix style violations in code being changed
  or identified during the requested lint work.
- Check error handling, boundary conditions, and the invariants affected by the change.
- Check that unsafe operations have a clear safety argument and that comments still
  describe the implementation.
- Review the final diff for accidental behavior changes and unnecessary complexity.
  For a review-only task, report actionable findings with file locations and impact.

`cargo fmt` and `cargo clippy` are not enough to enforce all project conventions.
Check item ordering, imports, address types, and documentation manually as well.
