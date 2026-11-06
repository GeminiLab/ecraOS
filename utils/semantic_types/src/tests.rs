//! Unit tests for generated expansion structure and rejected input.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemStruct, parse2};

use crate::{SemanticTypeArguments, semantic_type_impl};

/// Expands a struct with the supplied macro arguments for an assertion.
fn expand(args: TokenStream, input: TokenStream) -> String {
    let args = parse2::<SemanticTypeArguments>(args).unwrap();
    let input = parse2::<ItemStruct>(input).unwrap();
    semantic_type_impl(args, input).to_string()
}

/// Verifies that an input without a representation gets transparent representation.
#[test]
fn adds_transparent_representation_when_missing() {
    let output = expand(quote! {}, quote! { struct Count(u32); });

    assert!(output.contains("# [repr (transparent)]"), "{output}");
}

/// Verifies that an explicit representation is retained without adding another one.
#[test]
fn preserves_explicit_representation() {
    let output = expand(quote! {}, quote! { #[repr(C)] struct Count(u32); });

    assert!(output.contains("repr (C)"), "{output}");
    assert!(!output.contains("repr (transparent)"), "{output}");
}

/// Verifies that a named-field struct is rejected by the expansion.
#[test]
fn rejects_named_fields() {
    let output = expand(quote! {}, quote! { struct Count { value: u32 } });

    assert!(
        output.contains("Expected a single unnamed field"),
        "{output}"
    );
}

/// Verifies that a tuple struct with multiple fields is rejected by the expansion.
#[test]
fn rejects_multiple_tuple_fields() {
    let output = expand(quote! {}, quote! { struct Count(u32, u32); });

    assert!(
        output.contains("Expected a single unnamed field"),
        "{output}"
    );
}

/// Verifies that a non-integer field type is rejected by the expansion.
#[test]
fn rejects_non_integer_field_types() {
    let output = expand(quote! {}, quote! { struct Count(bool); });

    assert!(
        output.contains("Expected a primitive integer representation"),
        "{output}"
    );
}

/// Verifies that an unknown formatting option returns a parse error.
#[test]
fn rejects_unknown_formatting_options() {
    let error = syn::parse2::<SemanticTypeArguments>(quote! { binary })
        .err()
        .unwrap();

    assert!(error.to_string().contains("Unknown argument"), "{error}");
}

/// Verifies that disabling representation construction removes the generated constructor API.
#[test]
fn disables_repr_construction_generation() {
    let output = expand(quote! { no_from_repr }, quote! { struct Checked(u32); });

    assert!(!output.contains("pub const fn new"), "{output}");
    assert!(output.contains("From < Checked > for u32"), "{output}");
    assert!(!output.contains("From < u32 > for Checked"), "{output}");
}

/// Verifies that comparison and hashing options generate only their selected trait families.
#[test]
fn generates_only_selected_trait_families() {
    let output = expand(quote! { eq, ord_repr, hash }, quote! { struct Value(u32); });

    assert!(
        output.contains("derive (Debug , Copy , Clone , PartialEq , Eq , Hash)"),
        "{output}"
    );
    assert!(!output.contains("PartialOrd > for Value"), "{output}");
    assert!(!output.contains("Ord > for Value"), "{output}");
    assert!(output.contains("PartialEq < u32 > for Value"), "{output}");
    assert!(output.contains("PartialEq < Value > for u32"), "{output}");
    assert!(output.contains("PartialOrd < u32 > for Value"), "{output}");
    assert!(output.contains("PartialOrd < Value > for u32"), "{output}");
}

/// Verifies that wrapper comparisons and representation comparisons remain independent.
#[test]
fn comparison_options_are_independent() {
    let wrapper_eq = expand(quote! { eq }, quote! { struct Value(u32); });
    assert!(wrapper_eq.contains("PartialEq , Eq"), "{wrapper_eq}");
    assert!(!wrapper_eq.contains("PartialEq < u32 >"), "{wrapper_eq}");
    assert!(!wrapper_eq.contains("PartialOrd"), "{wrapper_eq}");

    let wrapper_ord = expand(quote! { ord }, quote! { struct Value(u32); });
    assert!(
        wrapper_ord.contains("PartialEq , Eq , PartialOrd , Ord"),
        "{wrapper_ord}"
    );
    assert!(!wrapper_ord.contains("PartialEq < u32 >"), "{wrapper_ord}");
    assert!(!wrapper_ord.contains("PartialOrd < u32 >"), "{wrapper_ord}");

    let repr_eq = expand(quote! { eq_repr }, quote! { struct Value(u32); });
    assert!(repr_eq.contains("PartialEq < u32 > for Value"), "{repr_eq}");
    assert!(!repr_eq.contains("PartialEq , Eq"), "{repr_eq}");
    assert!(!repr_eq.contains("PartialOrd"), "{repr_eq}");

    let repr_ord = expand(quote! { ord_repr }, quote! { struct Value(u32); });
    assert!(
        repr_ord.contains("PartialEq < u32 > for Value"),
        "{repr_ord}"
    );
    assert!(
        repr_ord.contains("PartialOrd < u32 > for Value"),
        "{repr_ord}"
    );
    assert!(
        !repr_ord.contains("derive (Debug , Copy , Clone , PartialEq"),
        "{repr_ord}"
    );
}
