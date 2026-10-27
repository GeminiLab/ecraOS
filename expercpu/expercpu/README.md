# `expercpu`

`expercpu` defines and accesses per-CPU data areas for kernel code. The crate is `no_std` by
default and supports a linker-provided per-CPU image that can be copied into one area for each CPU.

## Relationship to `percpu`

`expercpu` is ecraOS's upgraded version of the ArceOS
[`percpu`](https://github.com/arceos-org/percpu) crate. It keeps the same core model: per-CPU
values are collected into a `.percpu` image, each CPU receives an independent data area, and
current-CPU access combines the architecture-specific per-CPU register with the value's offset.
The upstream crate documents the same model together with preemption handling, remote access, and
the AArch64 `arm-el2` register selection.

The ecraOS version extends that model for this kernel's runtime and linker setup. The main changes
include the support for independently allocated, non-contiguous per-CPU areas, the adoption of
`memory_addr::VirtAddr` for the per-CPU area base, the exposure of the per-CPU image size and
allocation layout, the support for early-slot initialization, and the provision of a Linux host-test
backend. These additions make `expercpu` the ecraOS integration point rather than a drop-in path
replacement for the upstream crate.

## Per-CPU storage

Declare a per-CPU value with [`def_percpu`]. The macro places the value in the `.percpu` image and
generates a zero-sized accessor with the same name. The runtime must initialize one writable area
for each CPU with [`init`]. Areas may be allocated independently and do not need to be contiguous.

```rust,ignore
use expercpu::def_percpu;
use memory_addr::VirtAddr;

#[def_percpu]
static CURRENT_TASK: usize = 0;

fn initialize_current_cpu(base: VirtAddr) {
    // SAFETY: `base` names a writable, aligned area with the size returned by
    // `expercpu::percpu_area_layout`, and it does not overlap the linked image.
    unsafe { expercpu::init(base); }

    CURRENT_TASK.write_current(1);
    assert_eq!(CURRENT_TASK.read_current(), 1);
}
```

Primitive values such as `bool`, `u8`, `u16`, `u32`, `u64`, and `usize` provide
`read_current` and `write_current` helpers. All value types provide `with_current` and
`reset_to_init`. The raw pointer and reference methods are unsafe because callers must keep the
current CPU area valid and prevent CPU migration while the access is active.

[`percpu_area_size`] returns the size of the linked per-CPU image. [`percpu_area_layout`] returns a
64-byte-aligned allocation layout large enough for one area. [`init`] copies the image and installs
the architecture-specific current-CPU register.

## Optional features

- `host-test` provides a Linux-only backend that assigns one independent area to each host thread.
- `remote-access` adds remote pointer and reference methods and the `PerCPUAreaIf` interface used
  to resolve another CPU area.
- `preempt` makes safe current-CPU accessors hold a `kernel_guard::NoPreempt` guard. Without this
  feature, callers must provide the equivalent CPU-stability guarantee.
- `early-slot` exposes helpers for a linker-reserved early per-CPU area.
- `arm-el2` selects `TPIDR_EL2` on AArch64. The default AArch64 mode uses `TPIDR_EL1`.

The architecture-specific code generation currently covers x86-64, RISC-V 64, and AArch64. The
linker script for the final image must define `_percpu_start` and `_percpu_end`.
