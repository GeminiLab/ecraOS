# page_table_entry

[![Crates.io](https://img.shields.io/crates/v/page_table_entry)](https://crates.io/crates/page_table_entry)
[![Docs.rs](https://docs.rs/page_table_entry/badge.svg)](https://docs.rs/page_table_entry)
[![CI](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml)

This crate provides the definition of page table entry for various hardware
architectures.

Currently supported architectures and page table entry types:

- x86: [`x86_64::X64PTE`][1]
- AArch64: [`aarch64::A64PTE`][2]
- RISC-V: [`riscv::Rv64PTE`][3]
- LoongArch: [`loongarch64::LA64PTE`][4]

All these types implement the [`GenericPTE`][5] trait, which provides unified
methods for manipulating various page table entries.

Use [`GenericPTE::try_new_page`] and [`GenericPTE::try_set_flags`] to handle architecture-specific
flag errors. The existing `new_page` and `set_flags` convenience methods panic on these errors in
both debug and release builds. A failed `try_set_flags` leaves the entry unchanged. AArch64 requires
read permission for every leaf mapping. RISC-V rejects leaf entries without read or execute
permission and rejects write permission without read permission. Both implementations return the
original `MappingFlags` when rejecting a flag combination.

## Temporarily unsupported ARMv7

ARMv7-A (32-bit ARM) support is temporarily removed. Its Short-descriptor format requires the
translation-table level to distinguish L1 Section descriptors from L2 Small Page descriptors and
select the correct address mask and attribute layout. The current [`GenericPTE::paddr`],
[`GenericPTE::flags`], and [`GenericPTE::is_huge`] methods do not receive that context. The former
implementation guessed the descriptor kind from its address or type bits, which could truncate
physical addresses, misidentify page sizes, and decode or update the wrong fields.

The crate no longer exports an `arm` module or `A32PTE`. Restoring ARMv7 support requires passing
the translation-table level to descriptor operations or using distinct types for each level.

## RISC-V XuanTie memory attributes

The `xuantie-c9xx` feature encodes the SO, C, and B attributes shared by C906 and OpenC910.
The `xuantie-openc910` feature enables `xuantie-c9xx` and also sets SH for every mapped page.
The common feature alone leaves bit 60 clear, since C906 does not define SH.

| Mapping attributes | SO | C | B |
| --- | --- | --- | --- |
| Default cached normal memory | 0 | 1 | 1 |
| `UNCACHED` normal memory | 0 | 0 | 1 |
| `DEVICE` memory with buffered writes | 1 | 0 | 1 |

`DEVICE` takes precedence when both `DEVICE` and `UNCACHED` are set. `flags()` recovers the
corresponding memory type, so rebuilding a leaf retains its cache attributes. These features
encode PTE fields only. Platform code must enable `MXSTATUS.MAEE` for the processor to use them.
The vendor attributes are not the standard RISC-V Svpbmt encoding.

[1]: https://docs.rs/page_table_entry/latest/page_table_entry/x86_64/struct.X64PTE.html
[2]: https://docs.rs/page_table_entry/latest/page_table_entry/aarch64/struct.A64PTE.html
[3]: https://docs.rs/page_table_entry/latest/page_table_entry/riscv/struct.Rv64PTE.html
[4]: https://docs.rs/page_table_entry/latest/page_table_entry/loongarch64/struct.LA64PTE.html
[5]: https://docs.rs/page_table_entry/latest/page_table_entry/trait.GenericPTE.html

## Examples (x86_64)

```rust
use memory_addr::PhysAddr;
use x86_64::structures::paging::page_table::PageTableFlags;
use page_table_entry::{GenericPTE, MappingFlags, x86_64::X64PTE};

let paddr = PhysAddr::from(0x233000);
let pte = X64PTE::new_page(
    paddr,
    /* flags: */ MappingFlags::READ | MappingFlags::WRITE,
    /* is_huge: */ false,
);
assert!(!pte.is_unused());
assert!(pte.is_present());
assert_eq!(pte.paddr(), paddr);
assert_eq!(
    pte.bits(),
    0x800_0000000233_003, // PRESENT | WRITE | NO_EXECUTE | paddr(0x233000)
);
```
