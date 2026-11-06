#![doc = include_str!("../README.md")]

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::{
    Fields, FieldsUnnamed, Ident, ItemStruct, LitStr, Token, Type, TypePath, parenthesized,
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    token,
};

/// Unit tests for semantic type expansion and invalid input handling.
#[cfg(test)]
mod tests;

/// The options requested by one macro invocation.
///
/// The outer option distinguishes an omitted trait from a requested one. The nested option holds a
/// custom format string, or `None` when the trait uses its default format string.
struct SemanticTypeArguments {
    /// Disables construction and conversion from the representation.
    no_from_repr: bool,
    /// Enables equality between two semantic values.
    eq: bool,
    /// Enables equality between a semantic value and its representation.
    eq_repr: bool,
    /// Enables ordering between two semantic values.
    ord: bool,
    /// Enables ordering between a semantic value and its representation.
    ord_repr: bool,
    /// Enables hashing of a semantic value.
    hash: bool,
    /// The optional [`core::fmt::LowerHex`] implementation and its format string.
    lower_hex: Option<Option<String>>,
    /// The optional [`core::fmt::UpperHex`] implementation and its format string.
    upper_hex: Option<Option<String>>,
    /// The optional [`core::fmt::Display`] implementation and its format string.
    display: Option<Option<String>>,
}

/// Parses the options accepted by [`semantic_type`].
impl Parse for SemanticTypeArguments {
    /// Parses comma-separated formatting arguments.
    ///
    /// An unknown option or malformed value returns a syntax error.
    fn parse(input: ParseStream) -> syn::parse::Result<Self> {
        let mut args = SemanticTypeArguments {
            no_from_repr: false,
            eq: false,
            eq_repr: false,
            ord: false,
            ord_repr: false,
            hash: false,
            lower_hex: None,
            upper_hex: None,
            display: None,
        };

        /// Parses an optional parenthesized format string.
        ///
        /// An omitted string selects the default formatter for the requested trait.
        fn parse_option_string(input: ParseStream) -> syn::parse::Result<Option<String>> {
            if input.peek(token::Paren) {
                let in_paren;
                parenthesized!(in_paren in input);
                let value = in_paren.parse::<LitStr>()?;
                Ok(Some(value.value()))
            } else {
                Ok(None)
            }
        }

        loop {
            if input.is_empty() {
                break;
            }

            let ident = input.parse::<Ident>()?;
            match ident.to_string().as_str() {
                "no_from_repr" => args.no_from_repr = true,
                "eq" => args.eq = true,
                "eq_repr" => args.eq_repr = true,
                "ord" => args.ord = true,
                "ord_repr" => args.ord_repr = true,
                "hash" => args.hash = true,
                "lower_hex" => {
                    args.lower_hex = Some(parse_option_string(input)?);
                }
                "upper_hex" => {
                    args.upper_hex = Some(parse_option_string(input)?);
                }
                "display" => {
                    args.display = Some(parse_option_string(input)?);
                }
                _ => return Err(syn::Error::new(ident.span(), "Unknown argument")),
            }

            if input.is_empty() {
                break;
            }

            input.parse::<Token![,]>()?;
        }

        Ok(args)
    }
}

/// Reports whether a type path ends in a primitive signed or unsigned integer type.
fn is_primitive_integer_type(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, .. }) = ty {
        return path.segments.last().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
            )
        });
    }
    false
}

/// Generates one optional formatting trait implementation.
///
/// The formatter writes the represented value with the selected format string.
/// An absent format string produces no implementation.
fn generate_display_impl(
    display_trait: &Ident,
    target: &Ident,
    formatter: Option<&str>,
) -> TokenStream {
    match formatter {
        Some(formatter) => {
            quote! {
                #[doc = "Formats the semantic type using its represented value."]
                impl ::core::fmt::#display_trait for #target {
                    #[doc = "Writes the represented value using the configured format string."]
                    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                        write!(f, #formatter, self.into_repr())
                    }
                }
            }
        }
        None => quote! {},
    }
}

/// Generates bidirectional equality implementations with the representation.
fn generate_repr_eq_impl(target: &Ident, repr: &Type) -> TokenStream {
    quote! {
        #[doc = "Compares the semantic value with its representation."]
        impl ::core::cmp::PartialEq<#repr> for #target {
            #[doc = "Compares the represented value with a value of its representation type."]
            #[inline]
            fn eq(&self, other: &#repr) -> bool {
                self.into_repr() == *other
            }
        }

        #[doc = "Compares the representation with the semantic value."]
        impl ::core::cmp::PartialEq<#target> for #repr {
            #[doc = "Compares a value of the representation type with the represented value."]
            #[inline]
            fn eq(&self, other: &#target) -> bool {
                *self == other.into_repr()
            }
        }
    }
}

/// Generates bidirectional ordering implementations with the representation.
fn generate_repr_ord_impl(target: &Ident, repr: &Type) -> TokenStream {
    quote! {
        #[doc = "Orders the semantic value against its representation."]
        impl ::core::cmp::PartialOrd<#repr> for #target {
            #[doc = "Compares the represented value with a value of its representation type."]
            #[inline]
            fn partial_cmp(&self, other: &#repr) -> Option<::core::cmp::Ordering> {
                self.into_repr().partial_cmp(other)
            }
        }

        #[doc = "Orders the representation against the semantic value."]
        impl ::core::cmp::PartialOrd<#target> for #repr {
            #[doc = "Compares a value of the representation type with the represented value."]
            #[inline]
            fn partial_cmp(&self, other: &#target) -> Option<::core::cmp::Ordering> {
                self.partial_cmp(&other.into_repr())
            }
        }
    }
}

/// Generates the semantic type API for a single-field tuple struct.
///
/// Invalid field shapes and non-integer representation types become compile errors. A valid
/// expansion adds construction, representation access, conversions, selected comparisons, and the
/// requested formatting implementations.
fn semantic_type_impl(args: SemanticTypeArguments, input: ItemStruct) -> TokenStream {
    let name = input.ident.clone();

    // Check the field shape.
    let repr_type = if let Fields::Unnamed(FieldsUnnamed { unnamed, .. }) = &input.fields
        && unnamed.len() == 1
    {
        unnamed.first().unwrap().ty.clone()
    } else {
        return quote_spanned! {
            input.fields.span() => compile_error!("Expected a single unnamed field");
        };
    };

    // Check the underlying primitive type.
    if !is_primitive_integer_type(&repr_type) {
        let error_msg = format!(
            "Expected a primitive integer representation, got {:?}",
            repr_type
        );
        return quote_spanned! { repr_type.span() => compile_error!(#error_msg); };
    }

    // Check the repr attribute.
    let repr = if input
        .attrs
        .iter()
        .any(|attr| attr.meta.path().is_ident("repr"))
    {
        quote! {}
    } else {
        quote! { #[repr(transparent)] }
    };

    // Generate the formatting trait implementations.
    let lower_hex_impl = generate_display_impl(
        &format_ident!("LowerHex"),
        &name,
        args.lower_hex
            .as_ref()
            .map(|s| s.as_deref().unwrap_or("{:x}")),
    );
    let upper_hex_impl = generate_display_impl(
        &format_ident!("UpperHex"),
        &name,
        args.upper_hex
            .as_ref()
            .map(|s| s.as_deref().unwrap_or("{:X}")),
    );
    let display_impl = generate_display_impl(
        &format_ident!("Display"),
        &name,
        args.display.as_ref().map(|s| s.as_deref().unwrap_or("{:}")),
    );

    // Generate the derive attributes.
    let mut derives = vec![
        format_ident!("Debug"),
        format_ident!("Copy"),
        format_ident!("Clone"),
    ];
    if args.ord {
        derives.extend([
            format_ident!("PartialEq"),
            format_ident!("Eq"),
            format_ident!("PartialOrd"),
            format_ident!("Ord"),
        ]);
    } else if args.eq {
        derives.extend([format_ident!("PartialEq"), format_ident!("Eq")]);
    }
    if args.hash {
        derives.push(format_ident!("Hash"));
    }
    let derive_impl = quote! { #[derive(#(#derives),*)] };

    // Generate the methods.
    let new_impl = if args.no_from_repr {
        quote! {}
    } else {
        quote! {
            #[inline]
            pub const fn new(value: #repr_type) -> Self { Self(value) }
        }
    };
    let repr_from_impl = if args.no_from_repr {
        quote! {}
    } else {
        quote! {
            impl ::core::convert::From<#repr_type> for #name {
                #[inline]
                fn from(value: #repr_type) -> Self { Self::new(value) }
            }
        }
    };

    // Generate the comparison trait implementations.
    let repr_eq_impl = if args.eq_repr || args.ord_repr {
        generate_repr_eq_impl(&name, &repr_type)
    } else {
        quote! {}
    };
    let repr_ord_impl = if args.ord_repr {
        generate_repr_ord_impl(&name, &repr_type)
    } else {
        quote! {}
    };

    // The generated code.
    quote! {
        #repr
        #derive_impl
        #input

        #[doc = "Construction and representation access for the semantic type."]
        impl #name {
            #new_impl

            #[doc = concat!(
                "Returns the representation of this `",
                stringify!(#name),
                "` and consumes it."
            )]
            #[inline]
            pub const fn into_repr(self) -> #repr_type {
                self.0
            }
        }

        #repr_from_impl

        #[doc = "Conversion from the semantic type to its representation type."]
        impl ::core::convert::From<#name> for #repr_type {
            #[doc = "Consumes the semantic type and returns its representation."]
            #[inline]
            fn from(value: #name) -> Self {
                value.into_repr()
            }
        }

        #repr_eq_impl
        #repr_ord_impl

        #lower_hex_impl
        #upper_hex_impl
        #display_impl
    }
}

/// Generates a configurable API for a semantic type represented by a primitive integer.
///
/// The field must be a primitive integer type, and the struct must have exactly one unnamed field.
/// The macro always derives `Debug`, `Copy`, and `Clone`, adds `#[repr(transparent)]` when no `repr`
/// attribute is supplied, and generates `into_repr` and `From<Target> for Repr`. It generates `new`
/// and `From<Repr> for Target` by default. The `no_from_repr` option disables both construction
/// paths.
///
/// The `eq` option derives `PartialEq` and `Eq` for the semantic type. `ord` derives those traits
/// together with `PartialOrd` and `Ord`. The `eq_repr` option implements bidirectional equality
/// with the representation. `ord_repr` implements bidirectional partial ordering and implies
/// `eq_repr`. These groups are independent. The `hash` option derives `Hash` for the semantic type.
///
/// The `lower_hex`, `upper_hex`, and `display` options implement formatting traits. Each can take a
/// string literal in parentheses to customize its format string. Without one, the formats are
/// `{:x}`, `{:X}`, and `{:}`. The format string receives the representation as its first positional
/// argument. Formatting flags supplied to the outer formatting call are not forwarded. Formatting
/// reads through `into_repr`, which is available because generated types are `Copy`.
///
/// Do not derive a trait that the macro is configured to generate. See the crate documentation for
/// examples and the complete input contract.
#[proc_macro_attribute]
pub fn semantic_type(args: TokenStream1, input: TokenStream1) -> TokenStream1 {
    semantic_type_impl(
        parse_macro_input!(args as SemanticTypeArguments),
        parse_macro_input!(input as ItemStruct),
    )
    .into()
}
