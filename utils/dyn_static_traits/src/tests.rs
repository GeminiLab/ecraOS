use syn::parse_str;

use super::*;

/// Returns the implementation parameter name chosen for one trait.
///
/// The trait source must parse as a trait item.
fn chosen_name(source: &str) -> String {
    let trait_item = parse_str::<ItemTrait>(source).expect("trait should parse");
    implementation_type_ident(&trait_item).to_string()
}

/// Verifies that `T` is chosen when it is unused.
///
/// Lifetime parameters never conflict with the type parameter name.
#[test]
fn prefers_t() {
    assert_eq!(chosen_name("trait Clock<'a, V> { fn now() -> V; }"), "T");
}

/// Verifies that type parameters, const parameters, and the trait name are avoided.
///
/// The first unused candidate after `T` is chosen in shortest-first lexicographic order.
#[test]
fn skips_occupied_names() {
    assert_eq!(chosen_name("trait Wrap<T> {}"), "TA");
    assert_eq!(chosen_name("trait Size<const T: usize> {}"), "TA");
    assert_eq!(chosen_name("trait T {}"), "TA");
    assert_eq!(chosen_name("trait TA<T, const TB: usize> {}"), "TC");
}

/// Verifies that longer candidates follow every candidate of the previous length.
///
/// `TAA` is tried only after `T` and every name from `TA` to `TZ` are occupied.
#[test]
fn grows_after_exhausting_one_length() {
    let parameters = core::iter::once("T".to_owned())
        .chain((b'A'..=b'Z').map(|letter| format!("T{}", letter as char)))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        chosen_name(&format!("trait Many<{parameters}> {{}}")),
        "TAA"
    );
    let parameters = format!("{parameters}, TAA");
    assert_eq!(
        chosen_name(&format!("trait Many<{parameters}> {{}}")),
        "TAB"
    );
}

/// Verifies that raw identifiers are compared by their unescaped names.
///
/// A parameter written as `r#T` still occupies the name `T`.
#[test]
fn compares_raw_identifiers_by_name() {
    assert_eq!(chosen_name("trait Raw<r#T> {}"), "TA");
}
