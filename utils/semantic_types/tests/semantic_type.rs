//! Integration tests for the public `#[semantic_type]` attribute macro.

use semantic_types::semantic_type;

/// A unit-bearing count that uses the macro's default representation.
#[semantic_type]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Count(
    /// The wrapped count.
    u32,
);

/// A power value with the default formatting implementations.
#[semantic_type(lower_hex, upper_hex, display)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Power(
    /// The wrapped power value.
    u64,
);

/// A power value with custom formatting strings.
#[semantic_type(lower_hex("0x{:x}"), display("power={}"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FormattedPower(
    /// The wrapped power value.
    u64,
);

/// A count that supplies its own representation attribute.
#[semantic_type]
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CRepresentation(
    /// The wrapped count.
    u32,
);

/// A signed count formatted as decimal text.
#[semantic_type(display)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SignedCount(
    /// The wrapped signed count.
    i32,
);

/// Verifies construction, extraction, conversions, comparisons, and transparent layout.
#[test]
fn wraps_and_unwraps_the_inner_value() {
    let count = Count::new(17);

    assert_eq!(count.inner(), 17);
    assert_eq!(Count::from(17), count);
    assert_eq!(u32::from(count), 17);
    assert_eq!(count, 17);
    assert_eq!(17, count);
    assert_eq!(core::mem::size_of::<Count>(), core::mem::size_of::<u32>());
}

/// Verifies that bare formatting options use their documented defaults.
#[test]
fn formats_with_default_strings() {
    let power = Power::new(42);

    assert_eq!(format!("{power}"), "42");
    assert_eq!(format!("{power:x}"), "2a");
    assert_eq!(format!("{power:X}"), "2A");
    assert_eq!(format!("{power:#x}"), "2a");
}

/// Verifies that custom formatting strings receive the inner value.
#[test]
fn formats_with_custom_strings() {
    let power = FormattedPower::new(42);

    assert_eq!(format!("{power:x}"), "0x2a");
    assert_eq!(format!("{power}"), "power=42");
}

/// Verifies that the macro accepts a caller-supplied representation attribute.
#[test]
fn keeps_the_explicit_representation_usable() {
    let count = CRepresentation::new(17);

    assert_eq!(count.inner(), 17);
}

/// Verifies that signed primitive integer fields support formatting.
#[test]
fn formats_signed_values() {
    let count = SignedCount::new(-17);

    assert_eq!(format!("{count}"), "-17");
    assert_eq!(count.inner(), -17);
}
