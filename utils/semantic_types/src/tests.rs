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
        output.contains("Expected a primitive numeric type"),
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
