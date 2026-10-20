#![doc = include_str!("../README.md")]

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::{TokenStream, TokenTree};
use quote::quote;
use syn::{
    Ident, Lit, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    token::{Bracket, Paren},
};

/// The parsed product dimensions and optional callback.
///
/// An input contains zero or more literal dimensions followed by an optional callback invocation.
/// An input without dimensions expands to an empty array.
struct ProductInput {
    /// The literal dimensions in input order.
    ///
    /// Their order determines tuple element order and expansion order.
    dimensions: Punctuated<Dimension, Token![,]>,
    /// The optional callback invocation.
    ///
    /// It runs after the tuple array has been generated.
    callback: Option<Callback>,
}

/// A bracketed list of literal values.
///
/// Each value contributes one choice to every tuple generated from this dimension.
struct Dimension {
    /// The bracket tokens surrounding the values.
    ///
    /// This delimiter is retained for parsing and is not emitted separately.
    #[expect(unused)]
    brackets: Bracket,
    /// The values available in this dimension.
    ///
    /// Each value is cloned into every combination that selects it.
    values: Punctuated<Lit, Token![,]>,
}

/// An optional callback invocation after product expansion.
///
/// The callback receives each parsed argument after standalone @ arguments have been replaced
/// with the generated array.
struct Callback {
    /// The callback arrow.
    ///
    /// It separates the dimensions from the callback invocation.
    #[expect(unused)]
    arrow: Token![=>],
    /// The callback macro name.
    ///
    /// The name is emitted when constructing the callback invocation.
    callback: Ident,
    /// The macro invocation bang.
    ///
    /// It marks the following parenthesized tokens as macro arguments.
    #[expect(unused)]
    bang: Token![!],
    /// The parentheses surrounding the arguments.
    ///
    /// They delimit the callback arguments in the input.
    #[expect(unused)]
    paren: Paren,
    /// The comma-separated callback arguments.
    ///
    /// Each argument is considered independently when replacing standalone @ tokens.
    args: Punctuated<CallbackArg, Token![,]>,
}

/// A callback argument represented as tokens.
///
/// An argument is replaced only when its complete token sequence is the single token @.
struct CallbackArg {
    /// The tokens that make up the argument.
    ///
    /// The complete token sequence determines whether this argument is a replacement marker.
    value: TokenStream,
}

/// The parsing implementation for product input.
///
/// It reads bracketed dimensions followed by an optional callback invocation.
impl Parse for ProductInput {
    /// Parses dimensions and an optional callback from the input stream.
    ///
    /// A trailing comma after the final dimension is accepted.
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut dimensions = Punctuated::new();
        while input.peek(Bracket) {
            dimensions.push(input.parse()?);
            if !input.peek(Token![,]) {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        let callback = if input.peek(Token![=>]) {
            Some(input.parse()?)
        } else {
            None
        };
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after product input"));
        }
        Ok(Self {
            dimensions,
            callback,
        })
    }
}

/// The parsing implementation for a literal dimension.
///
/// It reads a bracketed, comma-separated list of literals.
impl Parse for Dimension {
    /// Parses one bracketed list of literal values.
    ///
    /// An empty list is accepted and causes the complete product to be empty.
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let content;
        let brackets = syn::bracketed!(content in input);
        Ok(Self {
            brackets,
            values: content.parse_terminated(Lit::parse, Token![,])?,
        })
    }
}

/// The parsing implementation for a callback invocation.
///
/// It reads a macro name followed by a parenthesized argument list.
impl Parse for Callback {
    /// Parses the callback macro name and its arguments.
    ///
    /// The callback name is a single identifier.
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let arrow = input.parse()?;
        let callback = input.parse()?;
        let bang = input.parse()?;
        let content;
        let paren = syn::parenthesized!(content in input);
        let args = content.parse_terminated(CallbackArg::parse, Token![,])?;
        Ok(Self {
            arrow,
            callback,
            bang,
            paren,
            args,
        })
    }
}

/// The parsing implementation for one callback argument.
///
/// It reads tokens up to the next top-level comma.
impl Parse for CallbackArg {
    /// Parses one non-empty callback argument.
    ///
    /// Delimited groups remain intact as individual token trees.
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut value = TokenStream::new();
        while !input.is_empty() && !input.peek(Token![,]) {
            value.extend([input.parse::<TokenTree>()?]);
        }
        if value.is_empty() {
            return Err(input.error("expected callback argument"));
        }
        Ok(Self { value })
    }
}

/// Expands literal dimensions into a Cartesian product array.
///
/// Each bracketed dimension is a comma-separated list of literals. The resulting array contains
/// one tuple for every combination, in input order, with the last dimension varying fastest. A
/// single dimension produces one-element tuples. With no dimensions or an empty dimension, the
/// result is an empty array. A trailing comma after the final dimension is accepted.
///
/// An optional `=> callback!(...)` suffix invokes a callback macro. Each callback argument that
/// is exactly `@` is replaced with the generated array. Other callback arguments are passed
/// through unchanged.
///
/// # Examples
///
/// Generate a tuple array from two dimensions:
///
/// ```
/// use cartesian_product::product;
///
/// let values: [(u8, u8); 4] = product!([1, 2], [3, 4]);
/// assert_eq!(values, [(1, 3), (1, 4), (2, 3), (2, 4)]);
/// ```
///
/// Pass the generated array to a callback macro with `@`:
///
/// ```
/// use cartesian_product::product;
///
/// macro_rules! capture {
///     ($values:tt, $label:ident) => {
///         ($values, stringify!($label))
///     };
/// }
///
/// let captured = product!([1, 2] => capture!(@, pairs));
/// assert_eq!(captured, ([(1,), (2,)], "pairs"));
/// ```
#[proc_macro]
pub fn product(input: TokenStream1) -> TokenStream1 {
    match syn::parse::<ProductInput>(input) {
        Ok(input) => expand(input).into(),
        Err(error) => error.into_compile_error().into(),
    }
}

/// Expands parsed dimensions and applies the optional callback.
///
/// It constructs the array before substituting it into callback arguments.
fn expand(input: ProductInput) -> TokenStream {
    let combinations = cartesian_product(&input.dimensions);
    let output = array_tokens(&combinations);
    match input.callback {
        Some(callback) => {
            let callback_name = callback.callback;
            let args = callback
                .args
                .iter()
                .map(|arg| replace_at(&arg.value, &output));
            quote! { #callback_name!(#(#args),*) }
        }
        None => output,
    }
}

/// Computes literal combinations in dimension order.
///
/// The last dimension varies fastest. No dimensions or an empty dimension produce no combinations.
fn cartesian_product(dimensions: &Punctuated<Dimension, Token![,]>) -> Vec<Vec<Lit>> {
    if dimensions.is_empty() {
        return Vec::new();
    }
    let mut combinations = vec![Vec::new()];
    for dimension in dimensions {
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                dimension.values.iter().cloned().map(move |value| {
                    let mut combination = prefix.clone();
                    combination.push(value);
                    combination
                })
            })
            .collect();
    }
    combinations
}

/// Renders each combination as a tuple in an array expression.
///
/// A one-value combination becomes a one-element tuple. An empty combination list becomes an
/// empty array expression.
fn array_tokens(combinations: &[Vec<Lit>]) -> TokenStream {
    let tuples = combinations.iter().map(|combination| {
        if combination.len() == 1 {
            let value = &combination[0];
            quote! { (#value,) }
        } else {
            quote! { (#(#combination),*) }
        }
    });
    quote! { [#(#tuples),*] }
}

/// Replaces a standalone @ argument with the generated product array.
///
/// Any other token sequence is returned unchanged.
fn replace_at(input: &TokenStream, output: &TokenStream) -> TokenStream {
    let mut tokens = input.clone().into_iter();
    match (tokens.next(), tokens.next()) {
        (Some(TokenTree::Punct(punct)), None) if punct.as_char() == '@' => output.clone(),
        _ => input.clone(),
    }
}
