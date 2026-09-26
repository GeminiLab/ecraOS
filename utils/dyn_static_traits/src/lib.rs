//! Static traits and runtime function-pointer tables.
//!
//! A static trait contains only associated functions without a receiver. Such a
//! trait can be used as a generic parameter for compile-time dependency
//! injection. The [`dyn_static_traits`] attribute adds dynamicization by
//! generating an explicit function-pointer table whose common type can be
//! stored and selected at runtime.

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{TokenStreamExt, quote, quote_spanned};
use syn::{Ident, ItemTrait, Signature, TraitItem, parse_macro_input, spanned::Spanned};

/// Creates a compile error attached to a syntax node.
///
/// The returned token stream highlights `span` and reports `msg` when the macro
/// expansion is compiled.
fn error_at<S: Spanned, M: AsRef<str> + ?Sized, T>(span: &S, msg: &M) -> Result<T, TokenStream> {
    let span = span.span();
    let msg = msg.as_ref();
    Err(quote_spanned!(span => compile_error!(#msg);))
}

/// Collects the static associated-method signatures from a trait.
///
/// A static trait contains only associated functions. Each function must have no
/// receiver, which means its declaration starts with `fn name(...)` instead of
/// `fn name(&self, ...)`, `fn name(&mut self, ...)`, or another receiver form.
///
/// Functions with receivers, generic parameters, asynchronous or variadic
/// signatures, and non-function trait items are rejected because they cannot be
/// represented by the generated function-pointer fields.
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

/// Expands a static trait and its function-pointer table.
///
/// `input_trait` must be a static trait, meaning that every associated method is
/// an associated function without a receiver. `output_ident` names the generated
/// table. Each table field corresponds to one static associated method, and the
/// generated constructor binds those fields to a selected trait implementation.
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
        let safety = signature.unsafety;
        let api = &signature.abi;
        let inputs = &signature.inputs;
        let output = &signature.output;

        let field_desc = format!(
            concat!(
                "Function pointer for [`{}::{}`].\n\n",
                "The pointer invokes this static associated function on the implementation selected\n",
                "when the table was constructed."
            ),
            trait_ident, fn_ident
        );

        fields.append_all(quote! {
            #[doc = #field_desc]
            pub #fn_ident: #safety #api fn(#inputs) #output,
        });

        init.append_all(quote! {
            #fn_ident: T::#fn_ident,
        });
    }

    let output_desc = format!(
        concat!(
            "A function-pointer table for [`{}`].\n\n",
            "Each field stores one static associated method from the implementation\n",
            "selected by [`Self::new`]. Each `new::<T>` call fixes its implementation\n",
            "statically, while the resulting table can be stored and selected at runtime\n",
            "as a Rust `dyn Trait` object.",
        ),
        trait_ident
    );

    Ok(quote! {
        #input_trait

        #[derive(Clone)]
        #[doc = #output_desc]
        #vis struct #output_ident #trait_generics {
            #fields
        }

        impl #impl_generics #output_ident #type_generics #where_clause {
            /// Creates a function-pointer table for a static-trait implementation.
            ///
            /// `T` supplies the static associated methods stored in the returned
            /// fields. The table can then be passed around independently of `T`.
            pub fn new<T: #trait_ident #type_generics>() -> Self {
                Self {
                    #init
                }
            }
        }
    })
}

/// Generates a function-pointer table for a static trait.
///
/// A static trait is a trait whose items are exclusively static associated
/// methods. In Rust terminology, these are associated functions with no receiver
/// in their signature. For example, `fn page_size() -> usize` is static, while
/// `fn page_size(&self) -> usize` is not.
///
/// Static traits are useful as generic parameters for static dependency
/// injection. A generic function such as `fn read<T: Storage>()` can call
/// `T::read()` with the implementation selected at compile time. This pattern
/// applies to many scenarios like allocators, logging, clocks, device access,
/// synchronization policies, platform services, and other static interfaces.
///
/// The generated table provides dynamic behavior for the same static interface.
/// It is a concrete struct of function pointers, not a `dyn Trait` object. Each
/// `new::<T>` call still names its implementation type statically, but the
/// resulting table no longer carries `T` in its own type. Tables built from
/// different implementations can therefore be stored behind one common type,
/// passed across an API boundary, or chosen conditionally at runtime.
///
/// The attribute argument names the generated table. The annotated trait must
/// contain only static associated methods. Those functions must not be generic,
/// asynchronous, or variadic, and the trait must not contain associated constants,
/// associated types, or other non-function items.
///
/// The generated table preserves each method's ABI and `unsafe` qualifier. A
/// field for an `unsafe fn` therefore still requires an unsafe call and retains
/// the safety contract of the original associated function.
///
/// # Examples
///
/// ```
/// use dyn_static_traits::dyn_static_traits;
///
/// #[dyn_static_traits(DynClock)]
/// trait Clock {
///     fn now() -> u64;
/// }
///
/// struct MonotonicClock;
///
/// impl Clock for MonotonicClock {
///     fn now() -> u64 {
///         42
///     }
/// }
///
/// // Static dependency injection uses the trait as a generic parameter.
/// fn read_time<T: Clock>() -> u64 {
///     T::now()
/// }
/// assert_eq!(read_time::<MonotonicClock>(), 42);
///
/// // Dynamic selection stores the generated function-pointer table.
/// let clock = DynClock::new::<MonotonicClock>();
/// assert_eq!((clock.now)(), 42);
/// ```
#[proc_macro_attribute]
pub fn dyn_static_traits(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    let output_ident = parse_macro_input!(attrs as Ident);
    let input_trait = parse_macro_input!(input as ItemTrait);

    dyn_static_traits_impl(input_trait, output_ident)
        .unwrap_or_else(|e| e)
        .into()
}
