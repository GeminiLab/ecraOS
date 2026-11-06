# `semantic_types`

`semantic_types` provides the [`semantic_type`] attribute macro for semantic types represented by
primitive integer types. It adds configurable construction and conversion APIs, optional comparison,
hashing, and formatting trait implementations to a single-field tuple struct.

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
#[semantic_type(eq_repr, ord, hash, lower_hex, upper_hex, display)]
struct Power(
    /// The underlying primitive representation.
    u64,
);

let power = Power::new(42);
assert_eq!(power.into_repr(), 42);
assert_eq!(u64::from(power), 42);
assert_eq!(Power::from(42), power);
assert_eq!(power, 42_u64);
assert_eq!(42_u64, power);
assert_eq!(format!("{power}"), "42");
assert_eq!(format!("{power:x}"), "2a");
assert_eq!(format!("{power:X}"), "2A");
```

Supported underlying primitive types are `u8`, `u16`, `u32`, `u64`, `u128`, `usize`, `i8`, `i16`, `i32`,
`i64`, `i128`, and `isize`. The struct must have exactly one unnamed field, whose visibility does
not matter. The macro preserves struct attributes and adds `#[repr(transparent)]` if the struct has
no explicit `repr` attribute.

Every annotated struct derives `Debug`, `Copy`, and `Clone` automatically. The macro always provides
a `const fn into_repr(self) -> Repr` method and `impl From<Target> for Repr` to retrieve the underlying
primitive representation. Here, `Repr` means the underlying primitive type, and `Target` means the
struct. By default, the generated code also provides `const fn new(Repr) -> Target` and `impl From<Repr>
for Target` to construct the struct from the underlying primitive representation.

## Options

The following bare options control construction, comparisons, hashing, and formatting:

| Option | Generated API |
| --- | --- |
| `no_from_repr` | Omits `fn new(Repr) -> Target` and `impl From<Repr> for Target`. |
| `eq` | Derives `PartialEq` and `Eq` between two `Target` values. |
| `eq_repr` | Implements bidirectional `PartialEq` between `Target` and `Repr` |
| `ord` | Derives `PartialOrd` and `Ord` between two `Target` values, and implies `eq`. |
| `ord_repr` | Implements bidirectional `PartialOrd` between `Target` and `Repr`, and implies `eq_repr`. |
| `hash` | Derives `Hash` for `Target`. |
| `lower_hex` | Implements `core::fmt::LowerHex`, using `{:x}` by default. |
| `upper_hex` | Implements `core::fmt::UpperHex`, using `{:X}` by default. |
| `display` | Implements `core::fmt::Display`, using `{:}` by default. |

Note that `ord` implies `eq`, and `ord_repr` implies `eq_repr`. But `eq` and `eq_repr` do not imply
each other, and `ord` and `ord_repr` do not imply each other.

```rust,compile_fail
use semantic_types::semantic_type;

/// A power value.
#[semantic_type(ord)]
struct Power(u64);

// error: Power is not PartialEq<u64>
assert_eq!(Power::new(42), 42u64);
```

Use `no_from_repr` when construction must validate a representation. Define a constructor on the
struct to enforce that validation:

```rust
use semantic_types::semantic_type;

/// A validated nonzero identifier.
#[semantic_type(no_from_repr)]
struct NonZeroId(u32);

impl NonZeroId {
    /// Constructs an identifier if its value is nonzero.
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}

assert!(NonZeroId::new(0).is_none());
assert_eq!(NonZeroId::new(7).unwrap().into_repr(), 7);
```

Formatting options can take a string literal in parentheses to customize their format string. The
represented value is passed as the first positional argument:

```rust
use semantic_types::semantic_type;

/// A formatted power value.
#[semantic_type(lower_hex("0x{:x}"), display("power={}"))]
struct FormattedPower(
    /// The represented power value.
    u64,
);

assert_eq!(format!("{:x}", FormattedPower::new(42)), "0x2a");
assert_eq!(format!("{}", FormattedPower::new(42)), "power=42");
```

Do not derive `Debug`, `Copy`, or `Clone` on the annotated type, or derive any trait that an option
generates. Other derives remain available. Unknown options, structs without exactly one unnamed
field, and fields that are not primitive integer types produce compile-time errors.
