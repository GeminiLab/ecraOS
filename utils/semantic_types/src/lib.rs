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

/// The optional formatting implementations requested by one macro invocation.
///
/// The outer option distinguishes an omitted trait from a requested one. The inner option holds a
/// custom format string, or `None` when the trait uses its default format string.
struct SemanticTypeArguments {
    /// The optional [`core::fmt::LowerHex`] implementation and its format string.
    lower_hex: Option<Option<String>>,
    /// The optional [`core::fmt::UpperHex`] implementation and its format string.
    upper_hex: Option<Option<String>>,
    /// The optional [`core::fmt::Display`] implementation and its format string.
    display: Option<Option<String>>,
}

/// Parses the formatting options accepted by [`semantic_type`].
impl Parse for SemanticTypeArguments {
    /// Parses comma-separated formatting arguments.
    ///
    /// An unknown option or malformed value returns a syntax error.
    fn parse(input: ParseStream) -> syn::parse::Result<Self> {
        let mut args = SemanticTypeArguments {
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
fn is_primitive_numeric_type(ty: &Type) -> bool {
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
/// The formatter writes the wrapped value with the selected format string. An absent format string
/// produces no implementation.
fn generate_display_impl(
    display_trait: &Ident,
    target: &Ident,
    formatter: Option<&str>,
) -> TokenStream {
    match formatter {
        Some(formatter) => {
            quote! {
                #[doc = "Formats the semantic type using its wrapped numeric value."]
                impl ::core::fmt::#display_trait for #target {
                    #[doc = "Writes the wrapped value using the configured format string."]
                    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                        write!(f, #formatter, self.inner())
                    }
                }
            }
        }
        None => quote! {},
    }
}

/// Generates the semantic type API for a single-field tuple struct.
///
/// Invalid field shapes and non-numeric field types become compile errors. A valid expansion adds
/// construction and value access, conversions, comparisons with the inner type, and the requested
/// formatting implementations.
fn semantic_type_impl(args: SemanticTypeArguments, input: ItemStruct) -> TokenStream {
    let name = input.ident.clone();
    let inner = if let Fields::Unnamed(FieldsUnnamed { unnamed, .. }) = &input.fields
        && unnamed.len() == 1
    {
        unnamed.first().unwrap().ty.clone()
    } else {
        return quote_spanned! {
            input.fields.span() => compile_error!("Expected a single unnamed field");
        };
    };

    if !is_primitive_numeric_type(&inner) {
        let error_msg = format!("Expected a primitive numeric type, got {:?}", inner);
        return quote_spanned! { inner.span() => compile_error!(#error_msg); };
    }

    let repr = if input
        .attrs
        .iter()
        .any(|attr| attr.meta.path().is_ident("repr"))
    {
        quote! {}
    } else {
        quote! { #[repr(transparent)] }
    };

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

    quote! {
        #repr
        #input

        #[doc = "Construction and value access for the semantic type."]
        impl #name {
            #[doc = concat!(
                "Constructs a new `",
                stringify!(#name),
                "` from a value of type `",
                stringify!(#inner),
                "`."
            )]
            #[inline]
            pub const fn new(value: #inner) -> Self {
                Self(value)
            }

            #[doc = concat!(
                "Returns the inner value of this `",
                stringify!(#name),
                "` and consumes it."
            )]
            #[inline]
            pub const fn inner(self) -> #inner {
                self.0
            }
        }

        #[doc = "Conversions between the semantic type and its inner numeric type."]
        impl ::core::convert::From<#inner> for #name {
            #[doc = "Wraps an inner numeric value in the semantic type."]
            #[inline]
            fn from(value: #inner) -> Self {
                Self::new(value)
            }
        }

        #[doc = "Conversions between the semantic type and its inner numeric type."]
        impl ::core::convert::From<#name> for #inner {
            #[doc = "Consumes the semantic type and returns its inner numeric value."]
            #[inline]
            fn from(value: #name) -> Self {
                value.inner()
            }
        }

        #[doc = "Comparisons between the semantic type and its inner numeric type."]
        impl ::core::cmp::PartialEq<#inner> for #name {
            #[doc = "Compares the wrapped value with an inner numeric value."]
            #[inline]
            fn eq(&self, other: &#inner) -> bool {
                self.inner() == *other
            }
        }

        #[doc = "Comparisons between the semantic type and its inner numeric type."]
        impl ::core::cmp::PartialEq<#name> for #inner {
            #[doc = "Compares an inner numeric value with the wrapped value."]
            #[inline]
            fn eq(&self, other: &#name) -> bool {
                *self == other.inner()
            }
        }

        #lower_hex_impl
        #upper_hex_impl
        #display_impl
    }
}

/// Generates a numeric newtype API for a single-field tuple struct.
///
/// The field must have a primitive integer type. The expansion adds `new` and `inner` methods,
/// conversions to and from the inner type, and comparisons with that type. It also adds
/// `#[repr(transparent)]` when the struct has no `repr` attribute.
///
/// The optional arguments `lower_hex`, `upper_hex`, and `display` implement the corresponding
/// formatting traits. Each can take a string literal in parentheses to customize the format string.
/// Without a string literal, the formats are `{:x}`, `{:X}`, and `{:}` respectively. The format
/// string receives the inner value as its first positional argument. Formatting flags supplied to
/// the outer formatting call are not forwarded to that string. A type that enables formatting must
/// implement [`Copy`] because the generated formatter calls `inner`, which consumes the wrapper.
///
/// See the crate documentation for examples and the complete input contract.
#[proc_macro_attribute]
pub fn semantic_type(args: TokenStream1, input: TokenStream1) -> TokenStream1 {
    semantic_type_impl(
        parse_macro_input!(args as SemanticTypeArguments),
        parse_macro_input!(input as ItemStruct),
    )
    .into()
}
