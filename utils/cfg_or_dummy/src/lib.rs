#![doc = include_str!("../README.md")]

use std::iter;

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use syn::{FnArg, Ident, ItemFn, Meta, Pat, parse_macro_input, parse_quote};

/// Collects identifiers bound by a function parameter pattern.
///
/// The supported pattern forms cover named parameters and the destructuring forms accepted by
/// `cfg_or_dummy`. Patterns without collectable identifiers contribute no bindings to the
/// fallback body.
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
        _ => Box::new(iter::empty()),
    }
}

/// Builds the conditional function expansion.
///
/// Emits the original function under `cond` and an unreachable dummy function under `not(cond)`.
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

/// Adds a condition-specific implementation and a dummy implementation for a free function.
///
/// The attribute accepts one `cfg` metadata item, such as `target_arch = "aarch64"` or
/// `any(target_arch = "aarch64", target_arch = "riscv64")`. For a function annotated with
/// `#[cfg_or_dummy(condition)]`, the macro emits two conditional definitions:
///
/// - `#[cfg(condition)]` emits the original function and its implementation.
/// - `#[cfg(not(condition))]` emits a dummy function with the same interface.
///
/// Only the definition whose condition matches the current configuration is compiled. The dummy
/// preserves the function's signature, visibility, attributes, generics, ABI, safety, and return
/// type. Its body is unreachable, so it does not implement the target-specific operation.
///
/// # Documentation builds
///
/// This attribute is intended for functions inside modules enabled with
/// `#[cfg(any(condition, doc))]`. During a documentation build for another target, `doc` keeps the
/// module available. A function marked only with `#[cfg(condition)]` would disappear from the
/// generated documentation, while an unconditionally compiled function could fail because its
/// body refers to target-specific code. The dummy keeps the function interface available while
/// avoiding that body.
///
/// The generated definitions receive `doc(cfg(condition))`, so rustdoc records the condition under
/// which the real implementation is available.
///
/// # Examples
///
/// Keeps an x86-64 inline-assembly implementation available in rustdoc:
///
/// ```
/// #[cfg(any(target_arch = "x86_64", doc))]
/// pub mod cpu {
///     use cfg_or_dummy::cfg_or_dummy;
///
///     #[cfg_or_dummy(target_arch = "x86_64")]
///     pub fn pause() {
///         // SAFETY: `pause` has no memory or stack effects exposed to Rust.
///         unsafe {
///             core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
///         }
///     }
/// }
///
/// #[cfg(target_arch = "x86_64")]
/// cpu::pause();
/// ```
#[proc_macro_attribute]
pub fn cfg_or_dummy(attr: TokenStream1, item: TokenStream1) -> TokenStream1 {
    let attr = parse_macro_input!(attr as Meta);
    let func = parse_macro_input!(item as ItemFn);

    cfg_or_dummy_impl(attr, func).into()
}
