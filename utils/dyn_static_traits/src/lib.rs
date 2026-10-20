#![doc = include_str!("../README.md")]

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{TokenStreamExt, format_ident, quote, quote_spanned};
use syn::{
    GenericParam, Ident, ItemTrait, Signature, TraitItem, ext::IdentExt, parse_macro_input,
    spanned::Spanned,
};

/// Unit tests for constructor parameter naming.
///
/// The tests parse trait items directly and do not invoke the attribute macro.
#[cfg(test)]
mod tests;

/// Creates a compile error attached to a syntax node.
///
/// The returned token stream highlights `span` and reports `msg` when the macro expansion is
/// compiled.
fn error_at<S: Spanned, M: AsRef<str> + ?Sized, T>(span: &S, msg: &M) -> Result<T, TokenStream> {
    let span = span.span();
    let msg = msg.as_ref();
    Err(quote_spanned!(span => compile_error!(#msg);))
}

/// Collects the static associated-method signatures from a trait.
///
/// A static trait contains only associated functions. Each function must have no receiver, which
/// means its declaration starts with `fn name(...)` instead of `fn name(&self, ...)`,
/// `fn name(&mut self, ...)`, or another receiver form.
///
/// Functions with receivers, generic parameters, asynchronous or variadic signatures, and
/// non-function trait items are rejected because they cannot be represented by the generated
/// function-pointer fields.
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

/// Chooses the implementation type parameter name for the generated constructor.
///
/// `T` is preferred. When the trait name or one of its type or const parameters already uses a
/// candidate, `T` followed by uppercase letters is tried next, from shorter to longer suffixes and
/// alphabetically among suffixes of one length, as in `TA`, `TB`, ..., `TZ`, `TAA`. Lifetime
/// parameters never conflict with the chosen name.
fn implementation_type_ident(trait_item: &ItemTrait) -> Ident {
    let occupied = trait_item
        .generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(param) => Some(&param.ident),
            GenericParam::Const(param) => Some(&param.ident),
            GenericParam::Lifetime(_) => None,
        })
        .chain([&trait_item.ident])
        .map(|ident| ident.unraw().to_string())
        .collect::<Vec<_>>();

    let mut suffix = Vec::new();
    loop {
        let candidate = core::iter::once('T')
            .chain(suffix.iter().map(|&letter| char::from(letter)))
            .collect::<String>();
        if !occupied.contains(&candidate) {
            return format_ident!("{}", candidate);
        }
        match suffix.iter().rposition(|&letter| letter != b'Z') {
            Some(index) => {
                suffix[index] += 1;
                suffix[index + 1..].fill(b'A');
            }
            None => {
                suffix.fill(b'A');
                suffix.push(b'A');
            }
        }
    }
}

/// Expands a static trait and its function-pointer table.
///
/// `input_trait` must be a static trait, meaning that every associated method is an associated
/// function without a receiver. `output_ident` names the generated table. Each table field
/// corresponds to one static associated method, and the generated constructor binds those fields to
/// a selected trait implementation.
fn dyn_static_traits_impl(
    input_trait: ItemTrait,
    output_ident: Ident,
) -> Result<TokenStream, TokenStream> {
    let vis = &input_trait.vis;
    let trait_ident = &input_trait.ident;
    let trait_generics = &input_trait.generics;
    let (impl_generics, type_generics, where_clause) = trait_generics.split_for_impl();
    let signatures = collect_static_trait_methods(&input_trait)?;
    let impl_ident = implementation_type_ident(&input_trait);

    let mut fields = TokenStream::new();
    let mut init = TokenStream::new();
    let mut clone_fields = TokenStream::new();

    for signature in signatures {
        let fn_ident = signature.ident.clone();
        let safety = signature.unsafety;
        let api = &signature.abi;
        let inputs = &signature.inputs;
        let output = &signature.output;

        let field_desc = format!(
            concat!(
                "The function pointer for [`{}::{}`].\n\n",
                "The pointer invokes this static associated function on the implementation\n",
                "selected when the table was constructed."
            ),
            trait_ident, fn_ident
        );

        fields.append_all(quote! {
            #[doc = #field_desc]
            pub #fn_ident: #safety #api fn(#inputs) #output,
        });

        init.append_all(quote! {
            #fn_ident: #impl_ident::#fn_ident,
        });

        clone_fields.append_all(quote! {
            #fn_ident: self.#fn_ident,
        });
    }

    let output_desc = format!(
        concat!(
            "A function-pointer table for [`{}`].\n\n",
            "Each field stores one static associated method from the implementation selected by\n",
            "[`Self::new`]. Each `new::<{}>` call fixes its implementation statically, while\n",
            "tables built from different implementations share this type and can be selected at\n",
            "runtime.",
        ),
        trait_ident, impl_ident
    );
    let new_desc = format!(
        concat!(
            "Creates a function-pointer table for a static-trait implementation.\n\n",
            "`{0}` supplies the static associated methods stored in the returned fields. The\n",
            "table can then be passed around independently of `{0}`.",
        ),
        impl_ident
    );

    Ok(quote! {
        #input_trait

        #[doc = #output_desc]
        #vis struct #output_ident #trait_generics {
            #fields
        }

        /// Constructs function-pointer tables from static-trait implementations.
        ///
        /// Each constructed table is independent of the implementation type that built it.
        impl #impl_generics #output_ident #type_generics #where_clause {
            #[doc = #new_desc]
            pub fn new<#impl_ident: #trait_ident #type_generics>() -> Self {
                Self {
                    #init
                }
            }
        }

        /// Clones function-pointer tables by copying their fields.
        ///
        /// Function pointers are always copyable, so the trait's type parameters need not
        /// implement `Clone`.
        impl #impl_generics ::core::clone::Clone for #output_ident #type_generics #where_clause {
            /// Returns a table whose fields point to the same functions.
            ///
            /// Each function pointer is copied from `self`.
            fn clone(&self) -> Self {
                Self {
                    #clone_fields
                }
            }
        }
    })
}

/// Generates a function-pointer table for a static trait.
///
/// The attribute is applied to a static trait and takes the name of the table to generate as its
/// only argument, as in `#[dyn_static_traits(DynClock)]`. The trait is emitted unchanged, so it
/// still works as a generic parameter, and the generated table adds runtime selection between its
/// implementations. The generated code needs no runtime support and works in `no_std` crates.
///
/// # Static traits
///
/// In this crate, a static trait means a trait whose items are exclusively static associated
/// methods, which Rust calls associated functions with no receiver in their signature. For example,
/// `fn page_size() -> usize` is static, while `fn page_size(&self) -> usize` is not.
///
/// Static traits are useful as generic parameters for static dependency injection. A generic
/// function such as `fn read<T: Storage>()` can call `T::read()` with the implementation selected
/// at compile time. This pattern applies to many scenarios like allocators, logging, clocks, device
/// access, synchronization policies, platform services, and other static interfaces.
///
/// Because their functions have no receiver, static traits are not dyn compatible, so a
/// `dyn Trait` object cannot select their implementation at runtime. The generated table provides
/// dynamic behavior for the same static interface instead. It is a concrete struct of function
/// pointers, not a `dyn Trait` object. Each `new::<T>` call still names its implementation type
/// statically, but the resulting table no longer carries `T` in its own type. Tables built from
/// different implementations can therefore be stored behind one common type, passed across an API
/// boundary, or chosen conditionally at runtime.
///
/// # Generated items
///
/// For `#[dyn_static_traits(DynClock)]` on a trait `Clock`, the macro emits the trait unchanged,
/// followed by these items:
///
/// - A struct `DynClock` with the visibility and generic parameters of `Clock`. It has one public
///   field per associated function, named after the function.
/// - An inherent `pub fn new<T: Clock>() -> Self` constructor that stores the functions of `T` in
///   the fields. A function that `T` does not override uses the default body from the trait.
/// - A `Clone` implementation that copies every function pointer. It places no `Clone` bound on
///   the parameters of the trait.
///
/// The table, its fields, its constructor, and its `Clone` implementation receive generated doc
/// comments. For a trait with one function, the expansion is roughly equivalent to the following
/// code:
///
/// ```ignore
/// trait Clock {
///     fn now() -> u64;
/// }
///
/// struct DynClock {
///     pub now: fn() -> u64,
/// }
///
/// impl DynClock {
///     pub fn new<T: Clock>() -> Self {
///         Self { now: T::now }
///     }
/// }
///
/// impl Clone for DynClock {
///     fn clone(&self) -> Self {
///         Self { now: self.now }
///     }
/// }
/// ```
///
/// ## Function-pointer fields
///
/// Each field type keeps the parameters, return type, ABI, and `unsafe` qualifier of its function,
/// so `unsafe extern "C" fn read(pointer: *const u32) -> u32;` produces a field of type
/// `unsafe extern "C" fn(pointer: *const u32) -> u32`. Elided lifetimes in a signature become
/// higher-ranked lifetimes of the function pointer, so a function such as
/// `fn first(text: &str) -> &str` keeps its borrowing behavior. A field for an `unsafe fn` still
/// requires an unsafe call and retains the safety contract of the original associated function.
///
/// A field is called with parentheses around the field access, such as `(table.now)()`, because
/// `table.now()` would look for a method.
///
/// ## Generic traits
///
/// Type, const, and lifetime parameters of the trait become parameters of the table, and `new`
/// requires its implementation parameter to implement the trait with the same arguments, as in
/// `DynStore::<u8, 4>::new::<Zeros>()`. A generic table is `Clone` regardless of its type
/// arguments. Every type and lifetime parameter of the trait must appear in at least one function
/// signature, because the generated struct would otherwise leave it unused.
///
/// The implementation parameter of `new` is named `T` when that name is free. When the trait
/// itself or one of its type or const parameters is named `T`, the macro uses the first free name
/// among `TA` to `TZ`, then `TAA`, `TAB`, and so on, so trait parameters can use any name.
///
/// # Requirements
///
/// The annotated trait must contain only static associated methods. The macro reports a compile
/// error for:
///
/// - functions with a `self` receiver,
/// - functions with generic parameters, including lifetime parameters,
/// - `async` or variadic functions,
/// - associated constants, associated types, macro invocations, and other non-function items.
///
/// Functions may have default bodies. Some signatures pass these checks, but a function-pointer
/// field cannot represent them, so the generated table fails to compile:
///
/// - signatures that mention `Self`, which refers to the table inside the generated struct,
/// - `impl Trait` in a parameter or return type,
/// - parameter patterns other than plain names and `_`, such as `mut value` or `(left, right)`,
///   even in functions with default bodies.
///
/// Attributes on trait functions stay on the trait and are not copied to the fields. In
/// particular, a function under a disabled `#[cfg]` still receives a field, so the constructor
/// fails to compile.
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
///
/// Tables built from different implementations share one type, so a program can choose between
/// them at runtime:
///
/// ```
/// use dyn_static_traits::dyn_static_traits;
///
/// #[dyn_static_traits(DynConsole)]
/// trait Console {
///     fn name() -> &'static str;
/// }
///
/// struct Serial;
///
/// impl Console for Serial {
///     fn name() -> &'static str {
///         "serial"
///     }
/// }
///
/// struct Framebuffer;
///
/// impl Console for Framebuffer {
///     fn name() -> &'static str {
///         "framebuffer"
///     }
/// }
///
/// fn select(graphics: bool) -> DynConsole {
///     if graphics {
///         DynConsole::new::<Framebuffer>()
///     } else {
///         DynConsole::new::<Serial>()
///     }
/// }
///
/// assert_eq!((select(false).name)(), "serial");
/// assert_eq!((select(true).name)(), "framebuffer");
/// ```
///
/// A generic trait produces a generic table, and default bodies fill the fields that an
/// implementation does not override:
///
/// ```
/// use dyn_static_traits::dyn_static_traits;
///
/// #[dyn_static_traits(DynStore)]
/// trait Store<V: Copy, const N: usize> {
///     fn read(index: usize) -> V;
///
///     fn capacity() -> usize {
///         N
///     }
/// }
///
/// struct Zeros;
///
/// impl Store<u8, 4> for Zeros {
///     fn read(_index: usize) -> u8 {
///         0
///     }
/// }
///
/// let store = DynStore::<u8, 4>::new::<Zeros>();
/// assert_eq!((store.read)(2), 0);
/// assert_eq!((store.capacity)(), 4);
/// ```
///
/// An `unsafe` associated function keeps its qualifier in the table:
///
/// ```
/// use dyn_static_traits::dyn_static_traits;
///
/// #[dyn_static_traits(DynReader)]
/// trait Reader {
///     /// Reads a value through a raw pointer.
///     ///
///     /// `pointer` must be valid for reads.
///     unsafe fn read(pointer: *const u32) -> u32;
/// }
///
/// struct DirectReader;
///
/// impl Reader for DirectReader {
///     unsafe fn read(pointer: *const u32) -> u32 {
///         // SAFETY: The caller guarantees that `pointer` is valid for reads.
///         unsafe { *pointer }
///     }
/// }
///
/// let reader = DynReader::new::<DirectReader>();
/// let value = 7;
/// // SAFETY: `value` is live for the duration of the call.
/// assert_eq!(unsafe { (reader.read)(&raw const value) }, 7);
/// ```
#[proc_macro_attribute]
pub fn dyn_static_traits(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    let output_ident = parse_macro_input!(attrs as Ident);
    let input_trait = parse_macro_input!(input as ItemTrait);

    dyn_static_traits_impl(input_trait, output_ident)
        .unwrap_or_else(|e| e)
        .into()
}
