use proc_macro::TokenStream as TokenStream1;
use proc_macro2::{TokenStream, TokenTree};
use quote::quote;
use syn::{
    Ident, Lit, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    token::{Bracket, Paren},
};

/// The parsed input for a Cartesian product expansion.
struct ProductInput {
    dimensions: Punctuated<Dimension, Token![,]>,
    callback: Option<Callback>,
}

/// A literal dimension in the product input.
struct Dimension {
    #[expect(unused)]
    brackets: Bracket,
    values: Punctuated<Lit, Token![,]>,
}

/// An optional callback invocation after product expansion.
struct Callback {
    #[expect(unused)]
    arrow: Token![=>],
    callback: Ident,
    #[expect(unused)]
    bang: Token![!],
    #[expect(unused)]
    paren: Paren,
    args: Punctuated<CallbackArg, Token![,]>,
}

/// One comma-delimited callback argument.
struct CallbackArg {
    value: TokenStream,
}

impl Parse for ProductInput {
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

impl Parse for Dimension {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let content;
        let brackets = syn::bracketed!(content in input);
        Ok(Self {
            brackets,
            values: content.parse_terminated(Lit::parse, Token![,])?,
        })
    }
}

impl Parse for Callback {
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

impl Parse for CallbackArg {
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

/// Expands literal dimensions into their Cartesian product.
///
/// An optional callback receives the generated array wherever an argument is exactly `@`.
#[proc_macro]
pub fn product(input: TokenStream1) -> TokenStream1 {
    match syn::parse::<ProductInput>(input) {
        Ok(input) => expand(input).into(),
        Err(error) => error.into_compile_error().into(),
    }
}

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

/// Computes all literal combinations in lexicographic dimension order.
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

/// Renders combinations as an array of tuple expressions.
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

/// Replaces an argument that is exactly `@` with the generated product array.
fn replace_at(input: &TokenStream, output: &TokenStream) -> TokenStream {
    let mut tokens = input.clone().into_iter();
    match (tokens.next(), tokens.next()) {
        (Some(TokenTree::Punct(punct)), None) if punct.as_char() == '@' => output.clone(),
        _ => input.clone(),
    }
}
