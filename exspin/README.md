# exspin

Kernel spin locks and reader-writer locks adapted to ecraOS's per-CPU storage.
This crate re-exports the crates.io version of kspin's base mutex and its raw
and preemption-only aliases. It provides its own BaseRwLock, SpinNoIrq,
RwLockNoIrq, and NoPreemptIrqSave.

## Shared IRQ state

Reader-writer guards do not necessarily form a stack: one CPU can acquire two
locks and release the first guard before the second. Saving IRQ flags in every
guard would enable interrupts while the second lock remains held, and could
leave IRQs disabled after both guards are gone.

All IRQ-safe locks and direct IRQ-save guards from exspin share a per-CPU
nesting depth and one outer saved-state token. They disable preemption before
accessing that state. The first acquisition retains kernel_guard's IRQ-save
token, and the last exit restores it after unlocking the protected data.
Guard destruction therefore supports non-LIFO order across lock types.

Each CPU must initialize expercpu before using IRQ-safe locks. The nesting
state must be inactive before changing or copying a CPU's per-CPU area.
Guards must remain on the acquiring CPU. These guards must not be mixed with
independently restored kernel_guard IRQ-save guards with overlapping lifetimes
that are not properly nested. Code must not manually enable IRQs or switch
tasks while an exspin IRQ-save guard is active. Non-maskable exceptions are
outside the synchronization guarantee.

## Reader-writer locks

The lock uses the reader-count and writer-bit design from spin. Readers may
overlap across CPUs. Writers exclude all readers and other writers. There is
no fairness guarantee, and a steady stream of readers can starve a writer.
Recursive acquisition, upgrading, and downgrading are not supported.
The atomic state is always present, including single-CPU kernels.

## Upstream direction

The intended direction is to move the per-CPU IRQ guard into kernel_guard and
the reader-writer lock into kspin. This currently requires adapting upstream
per-CPU support: ecraOS uses expercpu, its modified version of the percpu
library, rather than the percpu library normally used upstream. Their interfaces
and per-CPU storage conventions are not directly compatible.

## Host validation

Enable host-test to use expercpu's host backend when exercising actual IRQ-safe
lock aliases. Host kernel_guard does not execute privileged IRQ instructions,
so unit tests also use a simulated IRQ-save token to verify restoration timing.
