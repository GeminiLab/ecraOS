# `semantic_types`

`semantic_types` provides the [`semantic_type`] attribute macro for numeric newtypes. It
adds construction and inner-value access, conversions, comparisons with the inner value, and
optional formatting implementations to a single-field tuple struct.

## Usage

Add the crate as a dependency:

```toml
[dependencies]
semantic_types = "0.1"
```

Annotate a tuple struct whose field is a primitive integer type:

```rust
use semantic_types::semantic_type;

/// A power value.
#[semantic_type(lower_hex, upper_hex, display)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Power(
    /// The wrapped power value.
    u64,
);

let power = Power::new(42);
assert_eq!(power.inner(), 42);
assert_eq!(format!("{power}"), "42");
assert_eq!(format!("{power:x}"), "2a");
assert_eq!(format!("{power:X}"), "2A");
assert_eq!(Power::from(42), power);
assert_eq!(u64::from(power), 42);
assert_eq!(power, 42_u64);
assert_eq!(42_u64, power);
```

The supported field types are `u8`, `u16`, `u32`, `u64`, `u128`, `usize`, `i8`, `i16`, `i32`,
`i64`, `i128`, and `isize`. The struct must have exactly one unnamed field. The macro preserves
attributes on the struct and adds `#[repr(transparent)]` if the struct has no `repr` attribute.

The macro generates public `const fn new(inner)` and `const fn inner(self)` methods. The `inner`
method consumes the wrapper and returns its primitive value. It also implements
`From<Inner> for Wrapper`, `From<Wrapper> for Inner`, `PartialEq<Inner> for Wrapper`,
and `PartialEq<Wrapper> for Inner`. It does not derive traits or implement `PartialEq` between two
wrappers. Add the desired derives to the struct itself.

## Formatting

Formatting is opt-in. The bare options use the standard numeric format strings shown below:

| Argument | Trait | Default format string |
| --- | --- | --- |
| `lower_hex` | `core::fmt::LowerHex` | `{:x}` |
| `upper_hex` | `core::fmt::UpperHex` | `{:X}` |
| `display` | `core::fmt::Display` | `{:}` |

An option can take a custom format string. The wrapped value is passed as the first positional
argument to that string:

```rust
use semantic_types::semantic_type;

/// A formatted power value.
#[semantic_type(lower_hex("0x{:x}"), display("power={}"))]
#[derive(Clone, Copy)]
struct FormattedPower(
    /// The wrapped power value.
    u64,
);

assert_eq!(format!("{:x}", FormattedPower(42)), "0x2a");
assert_eq!(format!("{}", FormattedPower(42)), "power=42");
```

The formatting flags on the outer formatting call are not forwarded to the configured format
string. Put the desired formatting flags in the macro argument instead. A type that enables any
formatting option must implement `Copy` because the generated formatter calls
`inner(self)`, which consumes the wrapper.

Unknown arguments, structs without exactly one unnamed field, and fields that are not primitive
integer types produce compile-time errors.
