---
name: ecra-verify
description: Use when choosing or writing ecraOS tests, running format, lint, or validation checks, or preparing to report a task complete, including documentation-only work.
---

# ecraOS verification

## Choose checks

- Identify what could break and which observable behavior would demonstrate correctness.
- For code, build, or test changes, read [the testing guide](references/testing.md)
  and select checks for the affected behavior and targets.
- For bug fixes, reproduce the failure before changing the implementation and add
  a regression check when practical.
- Test behavior and invariants rather than duplicating implementation details.
  Include relevant boundary and failure cases. Reuse existing test infrastructure.
- For documentation-only work, check changed links, examples, and factual claims.
  Run executable examples or documentation tests when the change needs them.

## Execute and report

- Run the selected checks and inspect their output and exit status.
- Investigate failures before changing code or weakening assertions. Distinguish
  implementation failures from missing prerequisites and existing failures.
- After a fix, rerun affected checks. Expand validation when failures or wider impact
  justify it, rather than repeatedly running unrelated suites.
- Review the final diff and run `git diff --check`. Check newly created files too,
  since unstaged new files are absent from the ordinary diff.
- Report checks actually run and their results. Include relevant targets, profiles,
  or CPU counts, and explain omitted checks or remaining limitations.

A passing format check or successful build does not establish runtime correctness.
Do not describe an unexecuted check as passing.
