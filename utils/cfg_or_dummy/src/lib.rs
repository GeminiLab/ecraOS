use std::iter;

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use syn::{FnArg, Ident, ItemFn, Meta, Pat, parse_macro_input, parse_quote};

fn collect_ident_from_pat<'a>(pat: &'a Pat) -> Box<dyn Iterator<Item = &'a Ident> + 'a> {
    match pat {
        Pat::Ident(pat_ident) => Box::new(iter::once(&pat_ident.ident)),
        Pat::Paren(pat_paren) => collect_ident_from_pat(&pat_paren.pat),
        Pat::Reference(pat_reference) => collect_ident_from_pat(&pat_reference.pat),
        Pat::Slice(pat_slice) => Box::new(pat_slice.elems.iter().flat_map(collect_ident_from_pat)),
        Pat::Struct(pat_struct) => Box::new(
            pat_struct
                .fields
                .iter()
                .flat_map(|f| collect_ident_from_pat(&f.pat)),
        ),
        Pat::Tuple(pat_tuple) => Box::new(pat_tuple.elems.iter().flat_map(collect_ident_from_pat)),
        Pat::TupleStruct(pat_tuple_struct) => Box::new(
            pat_tuple_struct
                .elems
                .iter()
                .flat_map(collect_ident_from_pat),
        ),
        _ => todo!(),
    }
}

fn cfg_or_dummy_impl(cond: Meta, func: ItemFn) -> TokenStream {
    let params = func
        .sig
        .inputs
        .iter()
        .filter_map(|f| match f {
            FnArg::Typed(pat) => Some(pat),
            _ => None,
        })
        .flat_map(|p| collect_ident_from_pat(&p.pat));

    let mut func_unreachable = func.clone();
    func_unreachable.block = parse_quote!({
        #(
            let _ = #params;
        )*

        unreachable!()
    });

    quote::quote!(
        #[cfg(#cond)]
        #[cfg_attr(doc, doc(cfg(#cond)))]
        #func
        #[cfg(not(#cond))]
        #[cfg_attr(doc, doc(cfg(#cond)))]
        #func_unreachable
    )
}

#[proc_macro_attribute]
pub fn cfg_or_dummy(attr: TokenStream1, item: TokenStream1) -> TokenStream1 {
    let attr = parse_macro_input!(attr as Meta);
    let func = parse_macro_input!(item as ItemFn);

    cfg_or_dummy_impl(attr, func).into()
}
