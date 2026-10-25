# `expanic`

`expanic` provides panic and internal bug reporting for `no_std` applications such as kernels. It
offers the [`kpanic!`] and [`bug!`] macros, the [`UnwrapOrBug`] extension trait, and a replaceable
termination backend.

The crate requires nightly Rust for `extern_item_impls`. It does not define a panic handler or
provide platform shutdown code.

## Reporting termination

The [`kpanic!`] macro reports a fatal kernel error. The [`bug!`] macro reports an unexpected
internal error. Both macros call [`terminate`] with a formatted message and the module path, file,
line, and column of the macro call. They also mark the failure path as cold.

[`UnwrapOrBug`] adds [`unwrap_or_bug`](UnwrapOrBug::unwrap_or_bug) and
[`expect_or_bug`](UnwrapOrBug::expect_or_bug) to [`Option`] and [`Result`]. These methods use the
caller's file, line, and column through `#[track_caller]`. A module path is unavailable for method
calls.

```rust
use expanic::UnwrapOrBug;

assert_eq!(Some(42).unwrap_or_bug(), 42);
assert_eq!(Ok::<_, &str>(42).expect_or_bug("initialization failed"), 42);
```

## Replacing the termination backend

The default [`terminate`] implementation calls [`core::panic!`] with the supplied reason, source
location, and message. It is intended for host tests and standalone fallback use. Kernel code
should provide its own implementation with the
[`#[terminate_handler]`](terminate_handler) attribute so it can log the diagnostic and shut down the
platform.

The replacement must have the same signature as the default implementation and return `!`:

```rust
use core::fmt;

use expanic::{TerminateLocation, TerminateReason};

#[expanic::terminate_handler]
fn terminate(
    reason: TerminateReason,
    location: TerminateLocation<'_>,
    args: fmt::Arguments<'_>,
) -> ! {
    let _ = (reason, location, args);
    loop {}
}
```

When a replacement is registered, calls through `terminate` are dispatched to it. A kernel's
`#[panic_handler]` can forward ordinary Rust panics through `terminate` with
`TerminateReason::Panic` and `TerminateLocation::from_panic_location`. If no replacement is
registered, the default implementation calls `core::panic!`. A panic handler must not invoke that
fallback because it would start another panic.

[`TerminateReason`] distinguishes ordinary Rust panics, explicit kernel panics, and internal bugs.
[`TerminateLocation`] stores source metadata supplied by a macro or panic handler. The
termination backend decides whether to log, shut down, or apply another platform-specific policy.
