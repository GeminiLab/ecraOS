//! Integration tests for the public `#[semantic_type]` attribute macro.

use core::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};
use std::collections::hash_map::DefaultHasher;

use semantic_types::semantic_type;

/// A unit-bearing count that uses the macro's default representation.
#[semantic_type]
struct Count(
    /// The represented count.
    u32,
);

/// A power value with the default formatting implementations.
#[semantic_type(lower_hex, upper_hex, display)]
struct Power(
    /// The represented power value.
    u64,
);

/// A power value with custom formatting strings.
#[semantic_type(lower_hex("0x{:x}"), display("power={}"))]
struct FormattedPower(
    /// The represented power value.
    u64,
);

/// A count that supplies its own representation attribute.
#[semantic_type]
#[repr(C)]
struct CRepresentation(
    /// The represented count.
    u32,
);

/// A signed count formatted as decimal text.
#[semantic_type(display)]
struct SignedCount(
    /// The represented signed count.
    i32,
);

/// A type with wrapper equality enabled.
#[semantic_type(eq)]
struct Equality(u32);

/// A type with wrapper and representation equality enabled.
#[semantic_type(eq_repr)]
struct ReprEquality(u32);

/// A type with wrapper ordering and its implied equality enabled.
#[semantic_type(ord)]
struct OrderingValue(u32);

/// A type with representation ordering and its implied representation equality enabled.
#[semantic_type(ord_repr)]
struct ReprOrdering(u32);

/// A type with wrapper hashing enabled.
#[semantic_type(hash)]
struct Hashed(u32);

/// A type that must be constructed without the generated representation conversion.
#[semantic_type(no_from_repr)]
struct Checked(u32);

/// Verifies construction, extraction, conversions, comparisons, and transparent layout.
#[test]
fn wraps_and_extracts_the_representation() {
    let count = Count::new(17);

    assert_eq!(count.into_repr(), 17);
    assert_eq!(u32::from(count), 17);
    assert_eq!(core::mem::size_of::<Count>(), core::mem::size_of::<u32>());
}

/// Verifies that every generated type is debuggable, copyable, and clonable.
#[test]
fn generates_common_copy_traits() {
    fn assert_traits<T: core::fmt::Debug + Copy + Clone>() {}

    assert_traits::<Count>();
    assert_eq!(format!("{:?}", Count::new(17)), "Count(17)");
}

/// Verifies wrapper equality without enabling equality against the representation type.
#[test]
fn enables_wrapper_equality_independently() {
    assert_eq!(Equality::new(17), Equality::new(17));
    assert_ne!(Equality::new(17), Equality::new(18));
}

/// Verifies equality against the representation type in both operand orders.
#[test]
fn enables_repr_equality_independently() {
    assert_eq!(ReprEquality::new(17), 17);
    assert_eq!(17, ReprEquality::new(17));
}

/// Verifies that wrapper ordering implies wrapper equality.
#[test]
fn ordering_implies_wrapper_equality() {
    assert_eq!(
        OrderingValue::new(17).cmp(&OrderingValue::new(18)),
        Ordering::Less
    );
    assert_eq!(OrderingValue::new(17), OrderingValue::new(17));
}

/// Verifies that representation ordering implies representation equality in both operand orders.
#[test]
fn enables_repr_ordering_independently() {
    assert_eq!(ReprOrdering::new(17).partial_cmp(&18), Some(Ordering::Less));
    assert_eq!(
        18.partial_cmp(&ReprOrdering::new(17)),
        Some(Ordering::Greater)
    );
    assert_eq!(ReprOrdering::new(17), 17);
}

/// Verifies wrapper hashing is independently configurable.
#[test]
fn enables_wrapper_hashing_independently() {
    let mut first = DefaultHasher::new();
    let mut second = DefaultHasher::new();
    Hashed::new(17).hash(&mut first);
    Hashed::new(17).hash(&mut second);
    assert_eq!(first.finish(), second.finish());
}

/// Verifies the representation remains extractable when construction is restricted.
#[test]
fn disables_repr_construction() {
    let checked = Checked(17);
    assert_eq!(u32::from(checked), 17);
    assert_eq!(checked.into_repr(), 17);
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

/// Verifies that custom formatting strings receive the represented value.
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

    assert_eq!(count.into_repr(), 17);
}

/// Verifies that signed primitive integer fields support formatting.
#[test]
fn formats_signed_values() {
    let count = SignedCount::new(-17);

    assert_eq!(format!("{count}"), "-17");
    assert_eq!(count.into_repr(), -17);
}
