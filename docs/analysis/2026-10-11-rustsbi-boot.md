# RustSBI support on RISC-V64

Date: 2026-10-11

ecraOS's RISC-V loader already accepts the standard SBI boot interface. Using
RustSBI on QEMU therefore starts with replacing the firmware and checking
whether the kernel depends on behavior specific to OpenSBI.

The initial attempt reached the kernel but stopped during early memory
initialization. The failure exposed a missing initialization step: ecraOS
used floating-point instructions without enabling floating-point state. After
fixing both hart entry paths, the existing applications booted under RustSBI.

## Boot interface

RustSBI runs in machine mode and provides SBI services to supervisor-mode
software. ecraldr still loads and enters the ecraOS kernel. These roles give
the following boot path on QEMU `virt`:

```text
QEMU reset
  -> RustSBI at 0x80000000 in M-mode (-bios)
  -> ecraldr-riscv64-none at 0x80200000 in S-mode (-kernel)
  -> embedded ecraOS PIE kernel
```

The firmware is RustSBI Prototyper in dynamic mode. QEMU supplies the next-stage
entry address, so the firmware can be built independently of ecraldr and the
kernel. This fits the existing build flow, where each smoke application is
embedded in its own loader image.

At the supervisor entry, `a0` carries the boot hart ID and `a1` carries the
device-tree address. ecraldr already consumes these arguments. The kernel also
uses standard SBI calls for its console (DBCN), timer (TIME), secondary hart
startup (HSM), and shutdown (SRST). Those interfaces allow the same loader and
kernel to run with either firmware.

## Failure during early initialization

Both single-hart and four-hart runs initially stopped while printing the first
kernel text-section size. The kernel banner and preceding output showed that
the firmware handoff and console calls had worked. The failure occurred when
`size_disp` converted the section size to `f64` for formatting.

QEMU's exception trace repeatedly identified `fcvt.d.lu`, which converts an
unsigned integer to a double-precision value. The register dump showed an
illegal-instruction exception (`scause=2`) with `mstatus.FS=Off`.

The CPU supported floating-point instructions, as required by the
`riscv64gc-unknown-none-elf` target. However, the FS field controls whether the
current hart may access floating-point state. With FS set to Off, even a
supported floating-point instruction raises an illegal-instruction exception.
The same field is visible to supervisor software through `sstatus`.

ecraOS had relied on the floating-point state inherited from OpenSBI. The SBI
boot interface does not guarantee that FS is enabled, so the kernel needs to
establish that state itself.

## Entry initialization

The fix belongs in the assembly entry paths, before calling Rust. Initializing
FS later in a Rust function would still leave the preceding code dependent on
which instructions the compiler emits.

The BSP entry in `ecraldr/ecraldr-riscv64-none/src/boot.S` now performs:

```asm
li      t0, (3 << 13)            # sstatus.FS = Dirty
csrs    sstatus, t0
csrw    fcsr, zero
```

Setting FS to Dirty enables floating-point access. Clearing `fcsr` selects the
default rounding mode and clears floating-point exception flags. Initial and
Clean would also permit access. Dirty conservatively marks the inherited
floating-point state as modified.

FS is per-hart state, so initializing the BSP does not cover harts started
through HSM. The AP entry in `ecraos/exarch/src/arch/riscv64/mp.S` performs the
same initialization before entering Rust. Each hart must satisfy the target's
execution requirements, including access to floating-point state.

These entry changes establish the state needed to execute Rust code. Saving
and restoring floating-point registers across task switches remains a separate
context-management responsibility.

## Build and CI integration

`scripts/build-rustsbi.sh` builds the unmodified upstream Prototyper at
[commit c11e9d2](https://github.com/rustsbi/rustsbi/commit/c11e9d2c7850cc9672329e8a4f372214bd31d090),
which reports RustSBI 0.4.1. The script uses ecraOS's `nightly-2026-06-01`
toolchain. The source revision is fixed so an upstream firmware change cannot
silently alter a kernel regression test.

The firmware is selected through QEMU's `-bios` option, passed through xtask's
existing argument forwarding. From the repository root:

```bash
bash scripts/build-rustsbi.sh
cargo xtask qemu --target riscv64gc-unknown-none-elf \
    --crate smoke-apps/default,smoke-apps/sync-preemption,smoke-apps/sleep-join \
    --profile debug,release --cpus 1,4 --timeout 30 \
    --qemu-arg=-bios --qemu-arg=target/rustsbi/rustsbi-prototyper-dynamic.bin
```

The dedicated `rustsbi-tests` CI job builds the firmware once and runs this
matrix through xtask. It checks the firmware and kernel banners, application
completion, and entry on all three APs in four-hart runs. A failed cell makes
the job fail after xtask has run the remaining cells. Build and guest logs are
retained for diagnosis.

## Validation

Testing used QEMU 9.2.3 on the `virt` machine, with RustSBI built in release
mode. Each application ran in debug and release with one and four harts:

| Application | Behavior exercised |
| --- | --- |
| default | Early initialization, virtual memory, hart startup, and shutdown |
| sync-preemption | Timer preemption and blocking synchronization |
| sleep-join | Timer sleeps and task completion through join |

All 12 combinations completed and shut down successfully. The default
application also passed the four profile/hart-count combinations under
OpenSBI 1.5.1 after the entry changes.

The validation covers QEMU `virt`. Booting on a physical board also requires
firmware and hardware initialization appropriate to that board.
