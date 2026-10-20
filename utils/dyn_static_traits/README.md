# `dyn_static_traits`

`dyn_static_traits` provides the `#[dyn_static_traits]` attribute macro, which generates a
function-pointer table for a static trait so that its implementation can be chosen at runtime. The
generated code needs no runtime support, so it works in `no_std` crates.

## Static traits

In this crate, a static trait means a trait that contains only associated functions without a
receiver. Its functions are called on the implementing type, as in `C::now()`, rather than on a
value, so implementations are often unit structs. Passing an implementation as a generic parameter
selects it at compile time, which makes static traits a form of compile-time dependency injection:

```rust
trait Clock {
    fn now() -> u64;
}

struct BootClock;

impl Clock for BootClock {
    fn now() -> u64 {
        42
    }
}

fn elapsed_since<C: Clock>(start: u64) -> u64 {
    C::now() - start
}

assert_eq!(elapsed_since::<BootClock>(40), 2);
```

Because its functions have no receiver, a static trait is not dyn compatible, and `dyn Clock`
cannot select an implementation at runtime. The `#[dyn_static_traits]` attribute provides runtime
selection with a generated table of function pointers instead.

### Trait requirements

The attribute accepts only static traits and further restricts their functions. Default function
bodies are allowed, and the generated table uses a default body when an implementation does not
override it. The macro reports a compile error for:

- functions with a `self` receiver,
- functions with generic parameters, including lifetime parameters,
- `async` or variadic functions,
- associated constants, associated types, and other non-function items.

Some other signatures, such as those that mention `Self` or use `impl Trait`, cannot be represented
by function pointers either. The [attribute documentation](macro@dyn_static_traits) lists every
limitation.

## Usage

Add the crate as a dependency:

```toml
[dependencies]
dyn_static_traits = "0.1"
```

Then annotate a static trait with the attribute, passing the name of the generated table:

```rust
use dyn_static_traits::dyn_static_traits;

#[dyn_static_traits(DynClock)]
trait Clock {
    fn now() -> u64;
}

struct BootClock;

impl Clock for BootClock {
    fn now() -> u64 {
        42
    }
}

fn read_static<T: Clock>() -> u64 {
    T::now()
}

fn read_dynamic(clock: &DynClock) -> u64 {
    (clock.now)()
}

assert_eq!(read_static::<BootClock>(), 42);
assert_eq!(read_dynamic(&DynClock::new::<BootClock>()), 42);
```

The trait itself is emitted unchanged, so `read_static` still selects the implementation at compile
time, while `read_dynamic` receives it as a runtime value.

## Generated table

For a trait `Clock` and the argument `DynClock`, the macro generates a `DynClock` struct with the
trait's visibility and generic parameters, one public function-pointer field per associated
function, and a `Clone` implementation. Each field keeps the function's name, parameters,
return type, ABI, and `unsafe` qualifier, so calling a field of an `unsafe fn` still requires an
`unsafe` block. `DynClock::new::<T>()` builds a table whose fields point to the functions of the
implementation `T`. Tables built from different implementations share one type, so they can be
stored in structures, passed across API boundaries, and chosen at runtime.

Call a field with parentheses around the field access, such as `(clock.now)()`. Without them,
`clock.now()` looks for a method named `now`. The [attribute documentation](macro@dyn_static_traits)
describes the expansion, generic traits, and further examples.
