use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{TokenStreamExt, quote, quote_spanned};
use syn::{Ident, ItemTrait, Signature, TraitItem, parse_macro_input, spanned::Spanned};

fn error_at<S: Spanned, M: AsRef<str> + ?Sized, T>(span: &S, msg: &M) -> Result<T, TokenStream> {
    let span = span.span();
    let msg = msg.as_ref();
    Err(quote_spanned!(span => compile_error!(#msg);))
}

fn collect_static_trait_methods(trait_item: &ItemTrait) -> Result<Vec<&Signature>, TokenStream> {
    let mut result = Vec::with_capacity(trait_item.items.len());

    for item in trait_item.items.iter() {
        match item {
            TraitItem::Fn(fn_item) => {
                if !fn_item.sig.generics.params.is_empty() {
                    return error_at(
                        &fn_item.sig.generics,
                        "Associated functions must not have generic parameters",
                    );
                }

                if fn_item.sig.asyncness.is_some() {
                    return error_at(
                        &fn_item.sig.asyncness,
                        "Associated functions must not be async",
                    );
                }

                if fn_item.sig.variadic.is_some() {
                    return error_at(
                        &fn_item.sig.variadic,
                        "Associated functions must not be variadic",
                    );
                }

                if let Some(receiver) = fn_item.sig.receiver() {
                    return error_at(receiver, "Associated functions must not have a receiver");
                }

                result.push(&fn_item.sig);
            }
            _ => return error_at(item, "Static traits must only contain functions"),
        }
    }

    Ok(result)
}

fn dyn_static_traits_impl(
    input_trait: ItemTrait,
    output_ident: Ident,
) -> Result<TokenStream, TokenStream> {
    let vis = &input_trait.vis;
    let trait_ident = &input_trait.ident;
    let trait_generics = &input_trait.generics;
    let (impl_generics, type_generics, where_clause) = trait_generics.split_for_impl();
    let signatures = collect_static_trait_methods(&input_trait)?;

    let mut fields = TokenStream::new();
    let mut init = TokenStream::new();

    for signature in signatures {
        let fn_ident = signature.ident.clone();
        let api = &signature.abi;
        let inputs = &signature.inputs;
        let output = &signature.output;

        fields.append_all(quote! {
            pub #fn_ident: #api fn(#inputs) #output,
        });

        init.append_all(quote! {
            #fn_ident: T::#fn_ident,
        });
    }

    Ok(quote! {
        #input_trait

        #[derive(Clone)]
        #vis struct #output_ident #trait_generics {
            #fields
        }

        impl #impl_generics #output_ident #type_generics #where_clause {
            pub fn new<T: #trait_ident #type_generics>() -> Self {
                Self {
                    #init
                }
            }
        }
    })
}

#[proc_macro_attribute]
pub fn dyn_static_traits(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    let output_ident = parse_macro_input!(attrs as Ident);
    let input_trait = parse_macro_input!(input as ItemTrait);

    dyn_static_traits_impl(input_trait, output_ident)
        .unwrap_or_else(|e| e)
        .into()
}
