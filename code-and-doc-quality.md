# Next stage code and document quality improvement

This repo is now full of unreviewed code and missing or outdated documentation.
Since the implementation of 3 archs, task management, memory management and
interrupt handling (including ipi) is finished, it's time to improve the code
and document quality.

This document is a checklist for the next stage code and document quality
improvement, and should be removed after the improvement is finished.

## Target

### Code quality

For each crate:

- No warnings
- No clippy warnings or errors
- No format issues
- All testable code tested
- All tests pass
- Modular and clean code
- Good code (no duplication, no unnecessary complexity, etc.)

Commands to check code quality:

```
cargo fmt -p $crate -- --check
cargo clippy -p $crate -- -D warnings
cargo test -p $crate
```

## Documentation quality

For each crate:

- All **public and private** items (functions, types, modules, etc.) documented
- No wrong or outdated documentation
- No broken intra-doc links

Commands to check code quality:

```
RUSTFLAGS="--cfg ecraos_doc" RUSTDOCFLAGS="-Zunstable-options --enable-index-page --cfg ecraos_doc -D rustdoc::broken_intra_doc_links -D missing-docs" cargo doc -p $crate --no-deps --all-features --document-private-items
```

## Crate list

- [ ] [ecraos](ecraos)
- [ ] [exarch](exarch)
- [x] [exbuddy](exbuddy)
- [ ] [expalloc_trait](expalloc_trait)
- [x] [expanic](expanic)
- [ ] [expercpu/expercpu](expercpu/expercpu)
- [ ] [expercpu/expercpu_macros](expercpu/expercpu_macros)
- [x] [expt](expt)
- [ ] [exslab](exslab)
- [ ] [page_table_entry](page_table_entry)
- [x] [utils/cartesian_product](utils/cartesian_product)
- [x] [utils/cfg_or_dummy](utils/cfg_or_dummy)
- [x] [utils/dyn_static_traits](utils/dyn_static_traits)
- [x] [utils/look_at](utils/look_at)
- [x] [utils/maybe_non_generic](utils/maybe_non_generic)
- [x] [utils/memory_range_set](utils/memory_range_set)
- [x] [utils/size_disp](utils/size_disp)
