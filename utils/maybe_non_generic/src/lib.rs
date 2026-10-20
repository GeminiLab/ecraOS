#![doc = include_str!("../README.md")]

use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{
    AngleBracketedGenericArguments, Attribute, Block, Constraint, Expr, ExprCall, ExprField,
    ExprMethodCall, FnArg, GenericArgument, GenericParam, Generics, Ident, ImplItemFn, Item,
    ItemFn, Member, Pat, Path, PathArguments, PredicateType, Result, ReturnType, Signature, Token,
    TraitBound, Type, TypeParamBound, WherePredicate, parenthesized,
    parse::{Parse, ParseStream, Parser},
    parse_quote,
    punctuated::Punctuated,
    token::{Comma, Plus},
    visit::{self, Visit},
    visit_mut::{self, VisitMut},
};

/// Unit tests for argument parsing, signature generation, and body rewriting.
///
/// The tests call the expansion functions directly and inspect the generated tokens and errors.
#[cfg(test)]
mod tests;

/// The kind of a replaced generic parameter.
///
/// The kind decides how uses of the parameter are rewritten in the generated copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementKind {
    /// A const generic parameter.
    ///
    /// Its uses become the generated argument value.
    Const,
    /// A type generic parameter.
    ///
    /// Its associated items become fields of the generated argument.
    Type,
}

/// A generic parameter replaced by a trailing function parameter.
///
/// It is parsed from a `const(...)` or `type(...)` macro argument.
#[derive(Debug)]
struct Replacement {
    /// The kind of the replaced generic parameter.
    ///
    /// It must match the kind of the parameter in the function signature.
    kind: ReplacementKind,
    /// The replaced generic parameter.
    ///
    /// It is removed from the generics of the generated copy.
    param: Ident,
    /// The name of the generated function parameter.
    ///
    /// Rewritten uses of the replaced parameter refer to this name.
    arg_name: Ident,
    /// The type of the generated function parameter.
    ///
    /// Type replacements pass clones of a value of this type to rewritten calls.
    arg_type: Type,
}

/// A call target in a function replacement.
///
/// The source and target of one replacement always have the same form.
#[derive(Debug)]
enum FnTarget {
    /// A function path such as `Self::table_of`.
    ///
    /// Paths are matched by their segment names.
    Path(Path),
    /// A method on a receiver such as `self.clear`.
    ///
    /// Methods are matched by receiver tokens and method name.
    Method {
        /// The receiver expression of the method call.
        ///
        /// It is compared with call receivers by its token text.
        receiver: Expr,
        /// The method name.
        ///
        /// It is compared with the method name of each call.
        method: Ident,
    },
}

/// A rename of calls inside the generated copy.
///
/// It is parsed from a `fn(...)` macro argument.
#[derive(Debug)]
struct FnReplacement {
    /// The call target to match.
    ///
    /// Matching calls in the copied body are renamed.
    from: FnTarget,
    /// The call target to use instead.
    ///
    /// Only its names are used, and a renamed call keeps its other tokens.
    to: FnTarget,
}

/// The validated arguments of one macro invocation.
///
/// They describe the name, signature changes, body rewrites, and attributes of the generated copy.
#[derive(Debug)]
struct MacroArgs {
    /// The name of the generated copy.
    ///
    /// It replaces the original function name in the copied signature.
    output: Ident,
    /// The generic parameter replacements in signature order.
    ///
    /// Their generated parameters are appended to the copy in this order.
    replacements: Vec<Replacement>,
    /// The call renames applied to the copied body.
    ///
    /// Calls are compared with the renames in argument order.
    fn_replacements: Vec<FnReplacement>,
    /// Whether the original attributes are copied.
    ///
    /// The `dont_copy_attr` argument clears this flag.
    copy_attrs: bool,
    /// The attributes added to the copy.
    ///
    /// They follow any copied attributes.
    extra_attrs: Vec<Attribute>,
}

/// One parsed macro argument before validation.
///
/// Arguments are collected into [`MacroArgs`] after duplicate checks.
#[derive(Debug)]
enum MacroArg {
    /// A `const(...)` or `type(...)` argument.
    ///
    /// It replaces one generic parameter with a function parameter.
    Replacement(Replacement),
    /// A `fn(...)` argument.
    ///
    /// It renames matching calls in the copied body.
    FnReplacement(FnReplacement),
    /// The `dont_copy_attr` argument.
    ///
    /// It stops the original attributes from being copied.
    DontCopyAttr,
    /// An `attr(...)` argument.
    ///
    /// It adds one attribute to the copy.
    Attr(Attribute),
}

/// Generates a copy of a generic function with selected generic parameters replaced.
///
/// The attribute applies to a generic function with a body, which may be a free function, a
/// function in an `impl` block, or a default method of a trait. The original function is emitted
/// unchanged. The copy uses the name given by the first attribute argument, and each replaced
/// generic parameter becomes a trailing function parameter. A const generic becomes a runtime
/// value, and a type generic becomes a value that stands in for the type, such as a table of
/// function pointers.
///
/// For example, the following copy takes the number of summed values as its last argument:
///
/// ```
/// use maybe_non_generic::maybe_non_generic;
///
/// #[maybe_non_generic(sum_dyn, const(N => count: usize))]
/// fn sum<const N: usize>(values: &[u32]) -> u32 {
///     values.iter().take(N).sum()
/// }
///
/// assert_eq!(sum::<2>(&[1, 2, 3]), 3);
/// assert_eq!(sum_dyn(&[1, 2, 3], 2), 3);
/// ```
///
/// # Syntax
///
/// The first argument names the copy:
///
/// ```text
/// #[maybe_non_generic(copy_name, argument, ...)]
/// ```
///
/// The name can be followed by any number of comma-separated arguments, and a trailing comma is
/// allowed. Each argument has one of the following forms:
///
/// - `const(N => n: T)` replaces the const generic `N` with the trailing parameter `n: T`.
/// - `type(H => h: T)` replaces the type generic `H` with the trailing parameter `h: T`.
/// - `fn(path::f => path::g)` renames calls to the function path `path::f` in the copied body.
/// - `fn(receiver.f => receiver.g)` renames method calls `receiver.f(...)` in the copied body.
/// - `attr(...)` adds the attribute `#[...]` to the copy.
/// - `dont_copy_attr` stops the attributes of the original function from being copied.
///
/// # Generic parameter replacement
///
/// Each `const(...)` or `type(...)` argument names one generic parameter of the function and the
/// parameter that replaces it. The arguments must follow the order of the generic parameters in
/// the function signature, and the new parameters are appended in that order. Generic parameters
/// that are not replaced, including lifetimes, stay on the copy. Bounds and `where` predicates that
/// mention a replaced parameter are removed.
///
/// ## Body rewrites
///
/// The body of the copy is rewritten as follows:
///
/// - A const parameter `N` used as an expression becomes `n`.
/// - A call `H::f(args)` becomes `(h.f)(args)`, and an expression `H::ITEM` becomes `h.ITEM`. The
///   function-pointer tables generated by `dyn_static_traits` have this shape.
/// - In a call with a turbofish, such as `f::<N, H>(args)`, replaced generic arguments are removed
///   from the last path segment and appended to the call arguments. A const argument passes `n`,
///   and a type argument passes `::core::clone::Clone::clone(&h)`, so the type of `h` must
///   implement `Clone`. References and `dyn_static_traits` tables do.
///
/// Nested items, such as functions declared inside the body, are left unchanged, because they
/// cannot use the generic parameters of the outer function.
///
/// The original callee still expects its generic arguments, so a call that passes replaced
/// arguments usually also needs a `fn(...)` rename to the callee's own copy. The following example
/// replaces a clock type with a table generated by `dyn_static_traits` and renames the inner call:
///
/// ```
/// use dyn_static_traits::dyn_static_traits;
/// use maybe_non_generic::maybe_non_generic;
///
/// #[dyn_static_traits(DynClock)]
/// trait Clock {
///     fn now() -> u64;
/// }
///
/// struct BootClock;
///
/// impl Clock for BootClock {
///     fn now() -> u64 {
///         100
///     }
/// }
///
/// #[maybe_non_generic(elapsed_dyn, type(C => clock: &DynClock))]
/// fn elapsed<C: Clock>(start: u64) -> u64 {
///     C::now() - start
/// }
///
/// #[maybe_non_generic(
///     scaled_elapsed_dyn,
///     const(N => factor: u64),
///     type(C => clock: &DynClock),
///     fn(elapsed => elapsed_dyn),
/// )]
/// fn scaled_elapsed<const N: u64, C: Clock>(start: u64) -> u64 {
///     elapsed::<C>(start) * N
/// }
///
/// let clock = DynClock::new::<BootClock>();
/// assert_eq!(scaled_elapsed::<2, BootClock>(40), 120);
/// assert_eq!(scaled_elapsed_dyn(40, 2, &clock), 120);
/// ```
///
/// In `scaled_elapsed_dyn`, the call `elapsed::<C>(start)` becomes
/// `elapsed_dyn(start, ::core::clone::Clone::clone(&clock))`, and `N` becomes `factor`.
///
/// # Call renaming
///
/// A `fn(path => path)` argument renames calls whose function path has the same segment names as
/// the source path. Generic arguments are ignored while matching, and arguments that are not
/// replaced are kept, so `fn(Table::size => Table::size_dyn)` also matches
/// `Table::<M>::size::<L>(...)`. Both paths must have the same number of segments.
///
/// A `fn(receiver.method => receiver.method)` argument renames method calls whose receiver has the
/// same tokens as the given receiver, such as `self.clear(...)`. The source and target receivers
/// must be the same. Replaced generic arguments in the method turbofish are appended to the call
/// arguments.
///
/// The following example renames an associated function call and a method call in an `impl`
/// block:
///
/// ```
/// use maybe_non_generic::maybe_non_generic;
///
/// struct Table {
///     entries: usize,
/// }
///
/// impl Table {
///     #[maybe_non_generic(entry_size_dyn, const(LEVEL => level: usize))]
///     fn entry_size<const LEVEL: usize>() -> usize {
///         8 << LEVEL
///     }
///
///     #[maybe_non_generic(
///         bytes_dyn,
///         const(LEVEL => level: usize),
///         fn(Self::entry_size => Self::entry_size_dyn),
///     )]
///     fn bytes<const LEVEL: usize>(&self) -> usize {
///         Self::entry_size::<LEVEL>() * self.entries
///     }
///
///     #[maybe_non_generic(
///         double_bytes_dyn,
///         const(LEVEL => level: usize),
///         fn(self.bytes => self.bytes_dyn),
///     )]
///     fn double_bytes<const LEVEL: usize>(&self) -> usize {
///         self.bytes::<LEVEL>() * 2
///     }
/// }
///
/// let table = Table { entries: 4 };
/// assert_eq!(table.bytes::<1>(), 64);
/// assert_eq!(table.bytes_dyn(1), 64);
/// assert_eq!(table.double_bytes::<1>(), 128);
/// assert_eq!(table.double_bytes_dyn(1), 128);
/// ```
///
/// # Attributes
///
/// The copy receives the attributes of the original function, including doc comments, followed by
/// the attributes from `attr(...)` arguments. `#[maybe_non_generic]` attributes are never copied,
/// so stacking several of them on one function generates one copy per attribute. With
/// `dont_copy_attr`, only the `attr(...)` attributes are applied.
///
/// The following example generates two copies of one function with different doc comments:
///
/// ```
/// use maybe_non_generic::maybe_non_generic;
///
/// /// Sums the first `N` values.
/// #[maybe_non_generic(sum_dyn, const(N => count: usize))]
/// #[maybe_non_generic(
///     sum_counted,
///     const(N => count: usize),
///     dont_copy_attr,
///     attr(doc = "Sums the first `count` values."),
/// )]
/// fn sum<const N: usize>(values: &[u32]) -> u32 {
///     values.iter().take(N).sum()
/// }
///
/// assert_eq!(sum_dyn(&[1, 2, 3], 2), 3);
/// assert_eq!(sum_counted(&[1, 2, 3], 2), 3);
/// ```
///
/// `sum_dyn` receives the doc comment of `sum`, while `sum_counted` receives only the doc comment
/// from `attr(...)`.
///
/// # Limitations
///
/// The macro reports a compile error instead of generating a copy that might still depend on a
/// replaced parameter. It rejects:
///
/// - items other than functions with a body,
/// - functions without generic parameters,
/// - replacements that name a missing generic parameter, a parameter of the other kind, or the
///   same parameter twice,
/// - generated parameter names that are already bound by a function parameter or in the body,
/// - function parameter types or a return type that still mention a replaced parameter,
/// - generic arguments on an associated function of a replaced type, such as `H::f::<T>()`,
/// - replaced parameters that remain in the body after rewriting, for example in a type
///   annotation, in a generic argument expression such as `{ N - 1 }`, or in a qualified path such
///   as `<H as Trait>::f()`.
///
/// The macro cannot see inside some syntax, so it treats that syntax as a remaining use of a
/// replaced parameter. The body of the copy therefore cannot contain macro invocations, or
/// patterns with literals, ranges, or paths, such as `0`, `1..=9`, or `Ordering::Less`. This
/// applies even when the syntax does not mention a replaced parameter.
#[proc_macro_attribute]
pub fn maybe_non_generic(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    maybe_non_generic_impl(attrs.into(), input.into())
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

/// Expands the attribute arguments and annotated function into the original and its copy.
///
/// Free functions are tried before functions in `impl` blocks. Any other item is rejected.
fn maybe_non_generic_impl(attrs: TokenStream, input: TokenStream) -> Result<TokenStream> {
    let args = syn::parse2::<MacroArgs>(attrs)?;

    if let Ok(item) = syn::parse2::<ItemFn>(input.clone()) {
        return expand_item_fn(&args, item);
    }

    if let Ok(item) = syn::parse2::<ImplItemFn>(input) {
        return expand_impl_item_fn(&args, item);
    }

    Err(syn::Error::new(
        args.output.span(),
        "maybe_non_generic can only be applied to functions with bodies",
    ))
}

/// Returns whether an original attribute is copied to the generated copy.
///
/// Every attribute except `maybe_non_generic` is copied, so stacked invocations do not produce
/// copies of copies.
fn should_copy_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "maybe_non_generic")
}

/// Builds the attribute list of the generated copy.
///
/// The copied original attributes come first, followed by the attributes from `attr(...)`.
fn attrs_for_copy(original: &[Attribute], args: &MacroArgs) -> Vec<Attribute> {
    let mut attrs = Vec::new();

    if args.copy_attrs {
        attrs.extend(
            original
                .iter()
                .filter(|attr| should_copy_attr(attr))
                .cloned(),
        );
    }

    attrs.extend(args.extra_attrs.iter().cloned());
    attrs
}

/// Expands a free function into the original and its copy.
///
/// The copy receives the generated attributes, signature, and rewritten body.
fn expand_item_fn(args: &MacroArgs, item: ItemFn) -> Result<TokenStream> {
    let mut copy = item.clone();
    copy.attrs = attrs_for_copy(&item.attrs, args);
    copy.sig = signature_for_copy(&item.sig, args)?;
    transform_body(&mut copy.block, args)?;

    Ok(quote! {
        #item
        #copy
    })
}

/// Expands a function in an `impl` block into the original and its copy.
///
/// The copy receives the generated attributes, signature, and rewritten body.
fn expand_impl_item_fn(args: &MacroArgs, item: ImplItemFn) -> Result<TokenStream> {
    let mut copy = item.clone();
    copy.attrs = attrs_for_copy(&item.attrs, args);
    copy.sig = signature_for_copy(&item.sig, args)?;
    transform_body(&mut copy.block, args)?;

    Ok(quote! {
        #item
        #copy
    })
}

/// The parsing implementation for the macro arguments.
///
/// It reads the copy name followed by comma-separated arguments.
impl Parse for MacroArgs {
    /// Parses and validates all macro arguments.
    ///
    /// A trailing comma after the last argument is accepted.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let output = input.parse::<Ident>()?;
        let mut args = Vec::new();

        while input.parse::<Option<Token![,]>>()?.is_some() {
            if input.is_empty() {
                break;
            }

            args.push(input.parse::<MacroArg>()?);
        }

        validate_args(output, args)
    }
}

/// The parsing implementation for one macro argument.
///
/// It selects the argument form from its leading keyword or identifier.
impl Parse for MacroArg {
    /// Parses one `const`, `type`, `fn`, `attr`, or `dont_copy_attr` argument.
    ///
    /// Unknown identifiers are rejected.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        if input.peek(Token![const]) {
            input.parse::<Token![const]>()?;
            parse_replacement(input, ReplacementKind::Const).map(MacroArg::Replacement)
        } else if input.peek(Token![type]) {
            input.parse::<Token![type]>()?;
            parse_replacement(input, ReplacementKind::Type).map(MacroArg::Replacement)
        } else if input.peek(Token![fn]) {
            input.parse::<Token![fn]>()?;
            parse_fn_replacement(input).map(MacroArg::FnReplacement)
        } else {
            let name = input.parse::<Ident>()?;
            if name == "dont_copy_attr" {
                Ok(MacroArg::DontCopyAttr)
            } else if name == "attr" {
                parse_attr(input).map(MacroArg::Attr)
            } else {
                Err(syn::Error::new(
                    name.span(),
                    "unknown maybe_non_generic argument",
                ))
            }
        }
    }
}

/// Parses the parenthesized body of a `const(...)` or `type(...)` argument.
///
/// The body must contain exactly one `Param => name: Type` replacement.
fn parse_replacement(input: ParseStream<'_>, kind: ReplacementKind) -> Result<Replacement> {
    let content;
    parenthesized!(content in input);

    let param = content.parse::<Ident>()?;
    content.parse::<Token![=>]>()?;
    let arg_name = content.parse::<Ident>()?;
    content.parse::<Token![:]>()?;
    let arg_type = content.parse::<Type>()?;

    if !content.is_empty() {
        return Err(content.error("const/type argument must contain exactly one replacement"));
    }

    Ok(Replacement {
        kind,
        param,
        arg_name,
        arg_type,
    })
}

/// Parses the parenthesized body of a `fn(...)` argument.
///
/// The body must contain exactly one `source => target` replacement.
fn parse_fn_replacement(input: ParseStream<'_>) -> Result<FnReplacement> {
    let content;
    parenthesized!(content in input);

    let from = parse_fn_target(&content)?;
    content.parse::<Token![=>]>()?;
    let to = parse_fn_target(&content)?;

    if !content.is_empty() {
        return Err(content.error("fn argument must contain exactly one replacement"));
    }

    Ok(FnReplacement { from, to })
}

/// Parses one side of a function replacement.
///
/// Tokens are read up to the next `=>`. A field expression such as `self.clear` is a method
/// target, and anything else must be a path.
fn parse_fn_target(input: ParseStream<'_>) -> Result<FnTarget> {
    let target = input.step(|cursor| {
        let mut rest = *cursor;
        let mut tokens = TokenStream::new();

        while let Some((tt, next)) = rest.token_tree() {
            if matches!(&tt, proc_macro2::TokenTree::Punct(punct) if punct.as_char() == '=')
                && matches!(
                    next.token_tree(),
                    Some((proc_macro2::TokenTree::Punct(ref punct), _)) if punct.as_char() == '>'
                )
            {
                break;
            }

            tokens.extend(std::iter::once(tt));
            rest = next;
        }

        Ok((tokens, rest))
    })?;

    if let Ok(Expr::Field(ExprField {
        base,
        member: Member::Named(method),
        ..
    })) = syn::parse2::<Expr>(target.clone())
    {
        return Ok(FnTarget::Method {
            receiver: *base,
            method,
        });
    }

    syn::parse2::<Path>(target).map(FnTarget::Path)
}

/// Parses the parenthesized body of an `attr(...)` argument.
///
/// The body is the content of an outer attribute, such as `allow(dead_code)`.
fn parse_attr(input: ParseStream<'_>) -> Result<Attribute> {
    let content;
    parenthesized!(content in input);
    let content_tokens = content.parse::<TokenStream>()?;
    let attr_tokens = quote!(#[#content_tokens]);
    let mut attrs = Attribute::parse_outer.parse2(attr_tokens)?;
    attrs
        .pop()
        .ok_or_else(|| content.error("expected attribute contents"))
}

/// Collects parsed arguments into validated macro arguments.
///
/// Duplicate replaced parameters, duplicate generated names, and invalid function replacements
/// are rejected.
fn validate_args(output: Ident, args: Vec<MacroArg>) -> Result<MacroArgs> {
    let mut replacements = Vec::new();
    let mut fn_replacements = Vec::new();
    let mut copy_attrs = true;
    let mut extra_attrs = Vec::new();

    for arg in args {
        match arg {
            MacroArg::Replacement(replacement) => {
                if replacements
                    .iter()
                    .any(|existing: &Replacement| existing.param == replacement.param)
                {
                    return Err(syn::Error::new(
                        replacement.param.span(),
                        "duplicate replacement generic parameter",
                    ));
                }

                if replacements
                    .iter()
                    .any(|existing| existing.arg_name == replacement.arg_name)
                {
                    return Err(syn::Error::new(
                        replacement.arg_name.span(),
                        "duplicate generated argument name",
                    ));
                }

                replacements.push(replacement);
            }
            MacroArg::FnReplacement(replacement) => {
                validate_fn_replacement(&replacement)?;
                fn_replacements.push(replacement);
            }
            MacroArg::DontCopyAttr => copy_attrs = false,
            MacroArg::Attr(attr) => extra_attrs.push(attr),
        }
    }

    Ok(MacroArgs {
        output,
        replacements,
        fn_replacements,
        copy_attrs,
        extra_attrs,
    })
}

/// Checks that the source and target of a function replacement are compatible.
///
/// Both sides must be paths or methods, and method replacements must keep the same receiver.
fn validate_fn_replacement(replacement: &FnReplacement) -> Result<()> {
    match (&replacement.from, &replacement.to) {
        (
            FnTarget::Method {
                receiver: from_receiver,
                ..
            },
            FnTarget::Method {
                receiver: to_receiver,
                ..
            },
        ) => {
            if method_receiver_matches(from_receiver, to_receiver) {
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    to_receiver,
                    "method replacement target receiver must match source receiver",
                ))
            }
        }
        (FnTarget::Path(_), FnTarget::Path(_)) => Ok(()),
        (FnTarget::Path(_), FnTarget::Method { receiver, .. }) => Err(syn::Error::new_spanned(
            receiver,
            "function replacement source and target must have the same form",
        )),
        (FnTarget::Method { .. }, FnTarget::Path(path)) => Err(syn::Error::new_spanned(
            path,
            "function replacement source and target must have the same form",
        )),
    }
}

/// Finds the replacement for a parameter name.
///
/// The kind of the replacement is not checked.
fn replacement_for<'a>(ident: &Ident, replacements: &'a [Replacement]) -> Option<&'a Replacement> {
    replacements
        .iter()
        .find(|replacement| replacement.param == *ident)
}

/// Finds the replacement for a generic parameter.
///
/// Both the name and the parameter kind must match.
fn replacement_for_generic_param<'a>(
    param: &GenericParam,
    replacements: &'a [Replacement],
) -> Option<&'a Replacement> {
    replacements
        .iter()
        .find(|replacement| generic_param_matches_replacement(param, replacement))
}

/// Returns the name of a generic parameter.
///
/// Lifetime names are returned without the leading apostrophe.
fn generic_param_ident(param: &GenericParam) -> &Ident {
    match param {
        GenericParam::Lifetime(param) => &param.lifetime.ident,
        GenericParam::Type(param) => &param.ident,
        GenericParam::Const(param) => &param.ident,
    }
}

/// Checks the replacements against the generic parameters of a signature.
///
/// The signature must be generic, and each replacement must name an existing parameter of its kind
/// in signature order.
fn validate_replacements_against_signature(sig: &Signature, args: &MacroArgs) -> Result<()> {
    if sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.ident,
            "maybe_non_generic requires a function with generic parameters",
        ));
    }

    for replacement in &args.replacements {
        if sig
            .generics
            .params
            .iter()
            .any(|param| generic_param_matches_replacement(param, replacement))
        {
            continue;
        }

        if sig
            .generics
            .params
            .iter()
            .any(|param| generic_param_ident(param) == &replacement.param)
        {
            match replacement.kind {
                ReplacementKind::Const => {
                    return Err(syn::Error::new(
                        replacement.param.span(),
                        "const(...) must name a const generic parameter",
                    ));
                }
                ReplacementKind::Type => {
                    return Err(syn::Error::new(
                        replacement.param.span(),
                        "type(...) must name a type generic parameter",
                    ));
                }
            }
        } else {
            return Err(syn::Error::new(
                replacement.param.span(),
                "replacement generic parameter does not exist on the function",
            ));
        }
    }

    validate_replacement_order_matches_signature(sig, &args.replacements)
}

/// Checks that replacements are listed in signature order.
///
/// The order determines the order of the appended parameters.
fn validate_replacement_order_matches_signature(
    sig: &Signature,
    replacements: &[Replacement],
) -> Result<()> {
    let signature_order: Vec<&Replacement> = sig
        .generics
        .params
        .iter()
        .filter_map(|param| replacement_for_generic_param(param, replacements))
        .collect();

    for (actual, expected) in replacements.iter().zip(signature_order) {
        if actual.kind == expected.kind && actual.param == expected.param {
            continue;
        }

        return Err(syn::Error::new(
            actual.param.span(),
            "replacement generic parameters must be listed in function generic parameter order",
        ));
    }

    Ok(())
}

/// Returns whether a generic parameter is the target of a replacement.
///
/// Const replacements match const parameters, and type replacements match type parameters.
fn generic_param_matches_replacement(param: &GenericParam, replacement: &Replacement) -> bool {
    match (replacement.kind, param) {
        (ReplacementKind::Const, GenericParam::Const(param)) => param.ident == replacement.param,
        (ReplacementKind::Type, GenericParam::Type(param)) => param.ident == replacement.param,
        _ => false,
    }
}

/// Builds the signature of the generated copy.
///
/// The copy is renamed, replaced generic parameters and the predicates that mention them are
/// removed, and the generated parameters are appended.
fn signature_for_copy(original: &Signature, args: &MacroArgs) -> Result<Signature> {
    validate_replacements_against_signature(original, args)?;
    validate_generated_arg_names_against_inputs(original, &args.replacements)?;
    validate_existing_signature_types_do_not_use_replacements(original, &args.replacements)?;

    let mut sig = original.clone();
    sig.ident = args.output.clone();
    sig.generics = generics_for_copy(&sig.generics, &args.replacements);

    for replacement in &args.replacements {
        let arg_name = &replacement.arg_name;
        let arg_type = &replacement.arg_type;
        let pat: Pat = parse_quote!(#arg_name);
        sig.inputs.push(FnArg::Typed(parse_quote!(#pat: #arg_type)));
    }

    Ok(sig)
}

/// Checks that the existing signature types do not mention replaced parameters.
///
/// Such types would refer to a parameter that the copy no longer declares.
fn validate_existing_signature_types_do_not_use_replacements(
    sig: &Signature,
    replacements: &[Replacement],
) -> Result<()> {
    for input in &sig.inputs {
        let FnArg::Typed(input) = input else {
            continue;
        };

        if type_mentions_replaced_param(&input.ty, replacements) {
            return Err(syn::Error::new_spanned(
                &input.ty,
                "function parameter type still references a replaced generic parameter",
            ));
        }
    }

    if return_type_mentions_replaced_param(&sig.output, replacements) {
        return Err(syn::Error::new_spanned(
            &sig.output,
            "return type still references a replaced generic parameter",
        ));
    }

    Ok(())
}

/// Returns the segment names of a path.
///
/// Generic arguments are omitted.
fn path_segment_idents(path: &Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

/// Returns whether two paths have the same segment names.
///
/// Generic arguments on any segment are ignored.
fn paths_match_ignoring_turbofish(actual: &Path, expected: &Path) -> bool {
    path_segment_idents(actual) == path_segment_idents(expected)
}

/// Returns whether two receiver expressions have the same tokens.
///
/// The comparison is textual, so equivalent expressions written differently do not match.
fn method_receiver_matches(actual: &Expr, expected: &Expr) -> bool {
    actual.to_token_stream().to_string() == expected.to_token_stream().to_string()
}

/// Returns whether a type is a single-segment path with the given name.
///
/// Qualified paths and paths with more segments do not match.
fn is_type_ident(ty: &Type, ident: &Ident) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };

    type_path.qself.is_none()
        && type_path.path.segments.len() == 1
        && type_path.path.segments[0].ident == *ident
}

/// Returns whether an expression is a single-segment path with the given name.
///
/// Qualified paths and paths with more segments do not match.
fn expr_is_const_ident(expr: &Expr, ident: &Ident) -> bool {
    let Expr::Path(expr_path) = expr else {
        return false;
    };

    expr_path.qself.is_none()
        && expr_path.path.segments.len() == 1
        && expr_path.path.segments[0].ident == *ident
}

/// Returns the call argument that replaces a generic argument.
///
/// Const replacements pass the generated argument, and type replacements pass a clone of it.
fn replacement_expr(replacement: &Replacement) -> Expr {
    let arg_name = &replacement.arg_name;
    match replacement.kind {
        ReplacementKind::Const => parse_quote!(#arg_name),
        ReplacementKind::Type => parse_quote!(::core::clone::Clone::clone(&#arg_name)),
    }
}

/// Removes replaced generic arguments from an argument list.
///
/// Returns the call arguments that replace them, in order. Replaced parameters nested inside
/// other generic arguments are rejected.
fn remove_replaced_generic_args(
    args: &mut AngleBracketedGenericArguments,
    replacements: &[Replacement],
) -> Result<Vec<Expr>> {
    let mut appended = Vec::new();
    let mut kept = Punctuated::<GenericArgument, Comma>::new();

    for arg in args.args.iter().cloned() {
        let replacement = match &arg {
            GenericArgument::Type(ty) => replacements.iter().find(|replacement| {
                (replacement.kind == ReplacementKind::Type
                    || replacement.kind == ReplacementKind::Const)
                    && is_type_ident(ty, &replacement.param)
            }),
            GenericArgument::Const(expr) => replacements.iter().find(|replacement| {
                replacement.kind == ReplacementKind::Const
                    && expr_is_const_ident(expr, &replacement.param)
            }),
            _ => None,
        };

        if let Some(replacement) = replacement {
            appended.push(replacement_expr(replacement));
        } else if generic_argument_mentions_replaced_param(&arg, replacements) {
            return Err(syn::Error::new_spanned(
                arg,
                "replaced generic parameters inside generic arguments are unsupported",
            ));
        } else {
            kept.push(arg);
        }
    }

    args.args = kept;
    Ok(appended)
}

/// Removes replaced generic arguments from the last segment of a call path.
///
/// Returns the call arguments that replace them. Replaced parameters in the generic arguments of
/// earlier segments are rejected.
fn process_path_generic_args(path: &mut Path, replacements: &[Replacement]) -> Result<Vec<Expr>> {
    let final_index = path.segments.len().saturating_sub(1);
    let mut appended = Vec::new();

    for (index, segment) in path.segments.iter_mut().enumerate() {
        if let PathArguments::AngleBracketed(args) = &mut segment.arguments {
            if index == final_index {
                appended.extend(remove_replaced_generic_args(args, replacements)?);
                if args.args.is_empty() {
                    segment.arguments = PathArguments::None;
                }
            } else if angle_bracketed_args_mention_replaced_param(args, replacements) {
                return Err(syn::Error::new_spanned(
                    args.clone(),
                    "replaced generic parameters inside generic arguments are unsupported",
                ));
            }
        }
    }

    Ok(appended)
}

/// Reports a replaced parameter that remains in a rewritten body.
///
/// Returns `None` when no statement mentions a replaced parameter.
fn residual_replaced_param_error(
    block: &Block,
    replacements: &[Replacement],
) -> Option<syn::Error> {
    // TODO: analyze literal, range, and path patterns precisely, scan macro invocations for the
    // names of replaced parameters instead of rejecting every invocation, and report the offending
    // statement with a message that does not imply a parameter use.
    block
        .stmts
        .iter()
        .any(|stmt| stmt_mentions_replaced_param(stmt, replacements))
        .then(|| {
            syn::Error::new_spanned(
                block,
                "residual use of replaced generic parameter after body transformation",
            )
        })
}

/// Renames a call path according to the path replacements.
///
/// Segment names are replaced one by one, and generic arguments on the segments are kept.
fn rewrite_path_call(path: &mut Path, mappings: &[FnReplacement]) -> Result<()> {
    for mapping in mappings {
        let (FnTarget::Path(from), FnTarget::Path(to)) = (&mapping.from, &mapping.to) else {
            continue;
        };

        if !paths_match_ignoring_turbofish(path, from) {
            continue;
        }

        if path.segments.len() != to.segments.len() {
            return Err(syn::Error::new_spanned(
                path.clone(),
                "function path replacements must have the same number of segments",
            ));
        }

        for (segment, replacement_segment) in path.segments.iter_mut().zip(to.segments.iter()) {
            segment.ident = replacement_segment.ident.clone();
        }
    }

    Ok(())
}

/// A visitor that rewrites the body of the generated copy.
///
/// It collects errors while visiting, so several problems can be reported together.
struct BodyTransformer<'a> {
    /// The generic parameter replacements.
    ///
    /// They decide how parameter uses and generic arguments are rewritten.
    replacements: &'a [Replacement],
    /// The call renames.
    ///
    /// They are applied to function and method calls.
    fn_replacements: &'a [FnReplacement],
    /// The errors found while visiting.
    ///
    /// They are combined into one error after the visit.
    errors: Vec<syn::Error>,
}

/// Rewrites copied bodies and the calls inside them.
///
/// Each transformer is consumed by one body rewrite.
impl<'a> BodyTransformer<'a> {
    /// Creates a body transformer for the macro arguments.
    ///
    /// The transformer starts without errors.
    fn new(args: &'a MacroArgs) -> Self {
        Self {
            replacements: &args.replacements,
            fn_replacements: &args.fn_replacements,
            errors: Vec::new(),
        }
    }

    /// Rewrites a block and validates the result.
    ///
    /// Binding conflicts are checked before rewriting, and remaining uses of replaced parameters
    /// are checked after it.
    fn transform_block(mut self, block: &mut Block) -> Result<()> {
        if let Some(error) = body_binding_conflict_error(block, self.replacements) {
            self.errors.push(error);
        }

        self.visit_block_mut(block);
        if let Some(error) = residual_replaced_param_error(block, self.replacements) {
            self.errors.push(error);
        }

        if self.errors.is_empty() {
            Ok(())
        } else {
            let mut errors = self.errors.into_iter();
            let mut error = errors.next().expect("at least one transform error");
            for next in errors {
                error.combine(next);
            }
            Err(error)
        }
    }

    /// Finds the type replacement for an associated item path such as `H::f`.
    ///
    /// Returns the replacement and the item name. Generic arguments on the item are rejected.
    fn type_replacement_for_path(&self, path: &Path) -> Result<Option<(&Replacement, Ident)>> {
        if path.leading_colon.is_some() || path.segments.len() != 2 {
            return Ok(None);
        }

        let mut segments = path.segments.iter();
        let first = segments.next().expect("first path segment");
        let second = segments.next().expect("second path segment");

        if !matches!(first.arguments, PathArguments::None) {
            return Ok(None);
        }

        let Some(replacement) = self.replacements.iter().find(|replacement| {
            replacement.kind == ReplacementKind::Type && replacement.param == first.ident
        }) else {
            return Ok(None);
        };

        if !matches!(second.arguments, PathArguments::None) {
            return Err(syn::Error::new_spanned(
                second.arguments.clone(),
                "generic associated type-parameter access/calls are unsupported",
            ));
        }

        Ok(Some((replacement, second.ident.clone())))
    }

    /// Renames a method call according to the method replacements.
    ///
    /// Replaced generic arguments in the method turbofish are moved to the call arguments.
    fn rewrite_method_call(&mut self, node: &mut ExprMethodCall) {
        for mapping in self.fn_replacements {
            let (
                FnTarget::Method { receiver, method },
                FnTarget::Method {
                    method: to_method, ..
                },
            ) = (&mapping.from, &mapping.to)
            else {
                continue;
            };

            if node.method == *method && method_receiver_matches(&node.receiver, receiver) {
                node.method = to_method.clone();

                if let Some(turbofish) = &mut node.turbofish {
                    let appended = match remove_replaced_generic_args(turbofish, self.replacements)
                    {
                        Ok(appended) => appended,
                        Err(error) => {
                            self.errors.push(error);
                            return;
                        }
                    };
                    if turbofish.args.is_empty() {
                        node.turbofish = None;
                    }
                    node.args.extend(appended);
                }

                break;
            }
        }
    }
}

/// The mutable visiting implementation for body rewrites.
///
/// It rewrites calls, method calls, and paths, and visits other expressions normally.
impl VisitMut for BodyTransformer<'_> {
    /// Rewrites a function call expression.
    ///
    /// Arguments are visited first. A call through a replaced type parameter becomes a call of a
    /// field, and other path calls are renamed before their replaced generic arguments are moved.
    fn visit_expr_call_mut(&mut self, node: &mut ExprCall) {
        for arg in &mut node.args {
            self.visit_expr_mut(arg);
        }

        if let Expr::Path(expr_path) = &mut *node.func {
            if let Some((replacement, item)) = match self.type_replacement_for_path(&expr_path.path)
            {
                Ok(replacement) => replacement,
                Err(error) => {
                    self.errors.push(error);
                    return;
                }
            } {
                let arg_name = &replacement.arg_name;
                node.func = parse_quote!((#arg_name.#item));
                return;
            }

            if let Err(error) = rewrite_path_call(&mut expr_path.path, self.fn_replacements) {
                self.errors.push(error);
                return;
            }

            let appended = match process_path_generic_args(&mut expr_path.path, self.replacements) {
                Ok(appended) => appended,
                Err(error) => {
                    self.errors.push(error);
                    return;
                }
            };
            node.args.extend(appended);
        } else {
            self.visit_expr_mut(&mut node.func);
        }
    }

    /// Rewrites a method call expression.
    ///
    /// The receiver and arguments are visited before the method is renamed.
    fn visit_expr_method_call_mut(&mut self, node: &mut ExprMethodCall) {
        self.visit_expr_mut(&mut node.receiver);
        for arg in &mut node.args {
            self.visit_expr_mut(arg);
        }
        self.rewrite_method_call(node);
    }

    /// Rewrites an expression.
    ///
    /// Const parameters become their generated arguments, and associated items of replaced type
    /// parameters become fields of their generated arguments.
    fn visit_expr_mut(&mut self, node: &mut Expr) {
        match node {
            Expr::Call(call) => self.visit_expr_call_mut(call),
            Expr::MethodCall(call) => self.visit_expr_method_call_mut(call),
            Expr::Path(expr_path) => {
                if expr_path.qself.is_none() && expr_path.path.segments.len() == 1 {
                    let ident = &expr_path.path.segments[0].ident;
                    if let Some(replacement) = self.replacements.iter().find(|replacement| {
                        replacement.kind == ReplacementKind::Const && replacement.param == *ident
                    }) {
                        let arg_name = &replacement.arg_name;
                        *node = parse_quote!(#arg_name);
                        return;
                    }
                }

                if let Some((replacement, item)) =
                    match self.type_replacement_for_path(&expr_path.path) {
                        Ok(replacement) => replacement,
                        Err(error) => {
                            self.errors.push(error);
                            return;
                        }
                    }
                {
                    let arg_name = &replacement.arg_name;
                    *node = parse_quote!(#arg_name.#item);
                    return;
                }

                visit_mut::visit_expr_path_mut(self, expr_path);
            }
            _ => visit_mut::visit_expr_mut(self, node),
        }
    }

    /// Leaves a nested item unchanged.
    ///
    /// Nested items cannot use the generic parameters of the outer function, so names inside
    /// them refer to their own parameters or to other items.
    fn visit_item_mut(&mut self, _node: &mut Item) {}
}

/// Rewrites the body of the generated copy.
///
/// Errors from the rewrite and its validation are combined into one error.
fn transform_body(block: &mut Block, args: &MacroArgs) -> Result<()> {
    BodyTransformer::new(args).transform_block(block)
}

/// A visitor that finds body bindings with the same name as a generated parameter.
///
/// Such a binding would shadow the generated parameter in rewritten expressions.
struct BindingConflictVisitor<'a> {
    /// The generic parameter replacements.
    ///
    /// Their generated parameter names are compared with body bindings.
    replacements: &'a [Replacement],
    /// The first conflict found.
    ///
    /// Later conflicts are not recorded.
    error: Option<syn::Error>,
}

/// Checks blocks and patterns for conflicting bindings.
///
/// Only the first conflict is reported.
impl<'a> BindingConflictVisitor<'a> {
    /// Returns the first binding conflict in a block.
    ///
    /// Returns `None` when no pattern binds a generated parameter name.
    fn check(block: &'a Block, replacements: &'a [Replacement]) -> Option<syn::Error> {
        let mut visitor = Self {
            replacements,
            error: None,
        };
        visitor.visit_block(block);
        visitor.error
    }

    /// Records a conflict for a pattern that binds a generated parameter name.
    ///
    /// Nothing is recorded after the first conflict.
    fn check_pat(&mut self, pat: &'a Pat) {
        if self.error.is_some() {
            return;
        }

        if let Some(ident) = conflicting_pat_binding(pat, self.replacements) {
            self.error = Some(syn::Error::new(
                ident.span(),
                "body binding conflicts with a generated argument name",
            ));
        }
    }
}

/// The visiting implementation for binding conflict checks.
///
/// It checks every pattern in the body, including nested patterns.
impl<'ast> Visit<'ast> for BindingConflictVisitor<'ast> {
    /// Checks a pattern and then visits its subpatterns.
    ///
    /// Each nested identifier pattern is checked when it is visited.
    fn visit_pat(&mut self, node: &'ast Pat) {
        self.check_pat(node);
        visit::visit_pat(self, node);
    }

    /// Skips a nested item.
    ///
    /// Bindings inside nested items cannot shadow the parameters of the outer function.
    fn visit_item(&mut self, _node: &'ast Item) {}
}

/// Reports a body binding with the same name as a generated parameter.
///
/// Returns `None` when there is no conflict.
fn body_binding_conflict_error(block: &Block, replacements: &[Replacement]) -> Option<syn::Error> {
    BindingConflictVisitor::check(block, replacements)
}

/// Returns the binding of an identifier pattern that conflicts with a generated parameter.
///
/// Other patterns return `None`, because the visitor checks their subpatterns separately.
fn conflicting_pat_binding<'a>(pat: &'a Pat, replacements: &[Replacement]) -> Option<&'a Ident> {
    match pat {
        Pat::Ident(pat) => replacements
            .iter()
            .any(|replacement| replacement.arg_name == pat.ident)
            .then_some(&pat.ident),
        _ => None,
    }
}

/// Checks that generated parameter names are not already bound by function parameters.
///
/// Nested bindings in parameter patterns are included.
fn validate_generated_arg_names_against_inputs(
    sig: &Signature,
    replacements: &[Replacement],
) -> Result<()> {
    let mut existing_bindings = Vec::new();

    for input in &sig.inputs {
        let FnArg::Typed(input) = input else {
            continue;
        };
        collect_pat_bindings(&input.pat, &mut existing_bindings);
    }

    for existing in existing_bindings {
        if let Some(replacement) = replacements
            .iter()
            .find(|replacement| replacement.arg_name == *existing)
        {
            return Err(syn::Error::new(
                replacement.arg_name.span(),
                "generated argument name conflicts with an existing function parameter",
            ));
        }
    }

    Ok(())
}

/// Collects the identifiers bound by a pattern.
///
/// Nested patterns are searched recursively.
fn collect_pat_bindings<'a>(pat: &'a Pat, bindings: &mut Vec<&'a Ident>) {
    match pat {
        Pat::Ident(pat) => {
            bindings.push(&pat.ident);
            if let Some((_at, subpat)) = &pat.subpat {
                collect_pat_bindings(subpat, bindings);
            }
        }
        Pat::Or(pat) => {
            for case in &pat.cases {
                collect_pat_bindings(case, bindings);
            }
        }
        Pat::Paren(pat) => collect_pat_bindings(&pat.pat, bindings),
        Pat::Reference(pat) => collect_pat_bindings(&pat.pat, bindings),
        Pat::Slice(pat) => {
            for elem in &pat.elems {
                collect_pat_bindings(elem, bindings);
            }
        }
        Pat::Struct(pat) => {
            for field in &pat.fields {
                collect_pat_bindings(&field.pat, bindings);
            }
        }
        Pat::Tuple(pat) => {
            for elem in &pat.elems {
                collect_pat_bindings(elem, bindings);
            }
        }
        Pat::TupleStruct(pat) => {
            for elem in &pat.elems {
                collect_pat_bindings(elem, bindings);
            }
        }
        Pat::Type(pat) => collect_pat_bindings(&pat.pat, bindings),
        _ => {}
    }
}

/// Builds the generics of the generated copy.
///
/// Replaced parameters are removed, together with bounds and `where` predicates that mention
/// them.
fn generics_for_copy(generics: &Generics, replacements: &[Replacement]) -> Generics {
    let mut result = generics.clone();
    result.params = generics
        .params
        .iter()
        .filter(|param| replacement_for_generic_param(param, replacements).is_none())
        .cloned()
        .map(|mut param| {
            if let GenericParam::Type(param) = &mut param {
                param.bounds = retained_type_param_bounds(&param.bounds, replacements);
            }
            param
        })
        .collect();

    if let Some(where_clause) = &mut result.where_clause {
        where_clause.predicates = where_clause
            .predicates
            .iter()
            .filter(|predicate| !predicate_mentions_replaced_param(predicate, replacements))
            .cloned()
            .collect();
    }

    result
}

/// Returns the bounds of a retained type parameter that do not mention replaced parameters.
///
/// Bounds that the macro cannot analyze are removed.
fn retained_type_param_bounds(
    bounds: &Punctuated<TypeParamBound, Plus>,
    replacements: &[Replacement],
) -> Punctuated<TypeParamBound, Plus> {
    bounds
        .iter()
        .filter(|bound| !type_param_bound_mentions_replaced_param(bound, replacements))
        .cloned()
        .collect()
}

/// Returns whether a `where` predicate mentions a replaced parameter.
///
/// Predicate forms that the macro does not recognize are kept.
fn predicate_mentions_replaced_param(
    predicate: &WherePredicate,
    replacements: &[Replacement],
) -> bool {
    match predicate {
        WherePredicate::Type(predicate) => {
            type_predicate_mentions_replaced_param(predicate, replacements)
        }
        _ => false,
    }
}

/// Returns whether a type predicate mentions a replaced parameter.
///
/// Both the bounded type and its bounds are checked.
fn type_predicate_mentions_replaced_param(
    predicate: &PredicateType,
    replacements: &[Replacement],
) -> bool {
    type_mentions_replaced_param(&predicate.bounded_ty, replacements)
        || predicate
            .bounds
            .iter()
            .any(|bound| type_param_bound_mentions_replaced_param(bound, replacements))
}

/// Returns whether a type parameter bound mentions a replaced parameter.
///
/// Lifetime bounds never do, and bounds that the macro cannot analyze are treated as mentions.
fn type_param_bound_mentions_replaced_param(
    bound: &TypeParamBound,
    replacements: &[Replacement],
) -> bool {
    match bound {
        TypeParamBound::Trait(bound) => trait_bound_mentions_replaced_param(bound, replacements),
        TypeParamBound::Lifetime(_) => false,
        _ => true,
    }
}

/// Returns whether a trait bound mentions a replaced parameter.
///
/// The trait path and its generic arguments are checked.
fn trait_bound_mentions_replaced_param(bound: &TraitBound, replacements: &[Replacement]) -> bool {
    type_path_mentions_replaced_param(&bound.path, replacements)
}

/// Returns whether a type mentions a replaced parameter.
///
/// Types that the macro cannot analyze, such as macro invocations, are treated as mentions.
fn type_mentions_replaced_param(ty: &Type, replacements: &[Replacement]) -> bool {
    match ty {
        Type::Array(ty) => {
            type_mentions_replaced_param(&ty.elem, replacements)
                || expr_mentions_replaced_param(&ty.len, replacements)
        }
        Type::BareFn(ty) => {
            ty.inputs
                .iter()
                .any(|arg| type_mentions_replaced_param(&arg.ty, replacements))
                || return_type_mentions_replaced_param(&ty.output, replacements)
        }
        Type::Group(ty) => type_mentions_replaced_param(&ty.elem, replacements),
        Type::ImplTrait(ty) => ty
            .bounds
            .iter()
            .any(|bound| type_param_bound_mentions_replaced_param(bound, replacements)),
        Type::Infer(_) => false,
        Type::Macro(_) => true,
        Type::Never(_) => false,
        Type::Paren(ty) => type_mentions_replaced_param(&ty.elem, replacements),
        Type::Path(ty) => {
            ty.qself
                .as_ref()
                .is_some_and(|qself| type_mentions_replaced_param(&qself.ty, replacements))
                || type_path_mentions_replaced_param(&ty.path, replacements)
        }
        Type::Ptr(ty) => type_mentions_replaced_param(&ty.elem, replacements),
        Type::Reference(ty) => type_mentions_replaced_param(&ty.elem, replacements),
        Type::Slice(ty) => type_mentions_replaced_param(&ty.elem, replacements),
        Type::TraitObject(ty) => ty
            .bounds
            .iter()
            .any(|bound| type_param_bound_mentions_replaced_param(bound, replacements)),
        Type::Tuple(ty) => ty
            .elems
            .iter()
            .any(|elem| type_mentions_replaced_param(elem, replacements)),
        _ => true,
    }
}

/// Returns whether a return type mentions a replaced parameter.
///
/// The default unit return type never does.
fn return_type_mentions_replaced_param(
    return_type: &ReturnType,
    replacements: &[Replacement],
) -> bool {
    match return_type {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => type_mentions_replaced_param(ty, replacements),
    }
}

/// Returns whether an expression mentions a replaced parameter.
///
/// Expressions that the macro cannot analyze, such as macro invocations, are treated as mentions.
fn expr_mentions_replaced_param(expr: &Expr, replacements: &[Replacement]) -> bool {
    match expr {
        Expr::Array(expr) => expr
            .elems
            .iter()
            .any(|elem| expr_mentions_replaced_param(elem, replacements)),
        Expr::Assign(expr) => {
            expr_mentions_replaced_param(&expr.left, replacements)
                || expr_mentions_replaced_param(&expr.right, replacements)
        }
        Expr::Async(expr) => block_mentions_replaced_param(&expr.block, replacements),
        Expr::Await(expr) => expr_mentions_replaced_param(&expr.base, replacements),
        Expr::Binary(expr) => {
            expr_mentions_replaced_param(&expr.left, replacements)
                || expr_mentions_replaced_param(&expr.right, replacements)
        }
        Expr::Block(expr) => block_mentions_replaced_param(&expr.block, replacements),
        Expr::Break(expr) => expr
            .expr
            .as_ref()
            .is_some_and(|expr| expr_mentions_replaced_param(expr, replacements)),
        Expr::Call(expr) => {
            expr_mentions_replaced_param(&expr.func, replacements)
                || expr
                    .args
                    .iter()
                    .any(|arg| expr_mentions_replaced_param(arg, replacements))
        }
        Expr::Cast(expr) => {
            expr_mentions_replaced_param(&expr.expr, replacements)
                || type_mentions_replaced_param(&expr.ty, replacements)
        }
        Expr::Closure(expr) => {
            expr.inputs
                .iter()
                .any(|pat| pat_mentions_replaced_param(pat, replacements))
                || return_type_mentions_replaced_param(&expr.output, replacements)
                || expr_mentions_replaced_param(&expr.body, replacements)
        }
        Expr::Const(expr) => block_mentions_replaced_param(&expr.block, replacements),
        Expr::Continue(_) => false,
        Expr::Field(expr) => expr_mentions_replaced_param(&expr.base, replacements),
        Expr::ForLoop(expr) => {
            pat_mentions_replaced_param(&expr.pat, replacements)
                || expr_mentions_replaced_param(&expr.expr, replacements)
                || block_mentions_replaced_param(&expr.body, replacements)
        }
        Expr::Group(expr) => expr_mentions_replaced_param(&expr.expr, replacements),
        Expr::Index(expr) => {
            expr_mentions_replaced_param(&expr.expr, replacements)
                || expr_mentions_replaced_param(&expr.index, replacements)
        }
        Expr::If(expr) => {
            expr_mentions_replaced_param(&expr.cond, replacements)
                || expr
                    .then_branch
                    .stmts
                    .iter()
                    .any(|stmt| stmt_mentions_replaced_param(stmt, replacements))
                || expr.else_branch.as_ref().is_some_and(|(_, else_expr)| {
                    expr_mentions_replaced_param(else_expr, replacements)
                })
        }
        Expr::Infer(_) => false,
        Expr::Let(expr) => {
            pat_mentions_replaced_param(&expr.pat, replacements)
                || expr_mentions_replaced_param(&expr.expr, replacements)
        }
        Expr::Lit(_) => false,
        Expr::Loop(expr) => block_mentions_replaced_param(&expr.body, replacements),
        Expr::Macro(_) => true,
        Expr::Match(expr) => {
            expr_mentions_replaced_param(&expr.expr, replacements)
                || expr.arms.iter().any(|arm| {
                    pat_mentions_replaced_param(&arm.pat, replacements)
                        || arm.guard.as_ref().is_some_and(|(_if, guard)| {
                            expr_mentions_replaced_param(guard, replacements)
                        })
                        || expr_mentions_replaced_param(&arm.body, replacements)
                })
        }
        Expr::MethodCall(expr) => {
            expr_mentions_replaced_param(&expr.receiver, replacements)
                || expr.turbofish.as_ref().is_some_and(|args| {
                    angle_bracketed_args_mention_replaced_param(args, replacements)
                })
                || expr
                    .args
                    .iter()
                    .any(|arg| expr_mentions_replaced_param(arg, replacements))
        }
        Expr::Paren(expr) => expr_mentions_replaced_param(&expr.expr, replacements),
        Expr::Path(expr) => {
            expr.qself
                .as_ref()
                .is_some_and(|qself| type_mentions_replaced_param(&qself.ty, replacements))
                || expr_path_mentions_replaced_param(&expr.path, replacements)
        }
        Expr::Range(expr) => {
            expr.start
                .as_ref()
                .is_some_and(|expr| expr_mentions_replaced_param(expr, replacements))
                || expr
                    .end
                    .as_ref()
                    .is_some_and(|expr| expr_mentions_replaced_param(expr, replacements))
        }
        Expr::Reference(expr) => expr_mentions_replaced_param(&expr.expr, replacements),
        Expr::Repeat(expr) => {
            expr_mentions_replaced_param(&expr.expr, replacements)
                || expr_mentions_replaced_param(&expr.len, replacements)
        }
        Expr::Return(expr) => expr
            .expr
            .as_ref()
            .is_some_and(|expr| expr_mentions_replaced_param(expr, replacements)),
        Expr::Struct(expr) => {
            expr_path_mentions_replaced_param(&expr.path, replacements)
                || expr
                    .fields
                    .iter()
                    .any(|field| expr_mentions_replaced_param(&field.expr, replacements))
                || expr
                    .rest
                    .as_ref()
                    .is_some_and(|rest| expr_mentions_replaced_param(rest, replacements))
        }
        Expr::Tuple(expr) => expr
            .elems
            .iter()
            .any(|elem| expr_mentions_replaced_param(elem, replacements)),
        Expr::Try(expr) => expr_mentions_replaced_param(&expr.expr, replacements),
        Expr::TryBlock(expr) => block_mentions_replaced_param(&expr.block, replacements),
        Expr::Unary(expr) => expr_mentions_replaced_param(&expr.expr, replacements),
        Expr::Unsafe(expr) => block_mentions_replaced_param(&expr.block, replacements),
        Expr::While(expr) => {
            expr_mentions_replaced_param(&expr.cond, replacements)
                || block_mentions_replaced_param(&expr.body, replacements)
        }
        Expr::Yield(expr) => expr
            .expr
            .as_ref()
            .is_some_and(|expr| expr_mentions_replaced_param(expr, replacements)),
        _ => true,
    }
}

/// Returns whether a block mentions a replaced parameter.
///
/// Every statement in the block is checked.
fn block_mentions_replaced_param(block: &syn::Block, replacements: &[Replacement]) -> bool {
    block
        .stmts
        .iter()
        .any(|stmt| stmt_mentions_replaced_param(stmt, replacements))
}

/// Returns whether a statement mentions a replaced parameter.
///
/// Nested items are skipped because they cannot use outer generic parameters, and macro
/// statements are treated as mentions.
fn stmt_mentions_replaced_param(stmt: &syn::Stmt, replacements: &[Replacement]) -> bool {
    match stmt {
        syn::Stmt::Local(stmt) => {
            pat_mentions_replaced_param(&stmt.pat, replacements)
                || stmt
                    .init
                    .as_ref()
                    .is_some_and(|init| expr_mentions_replaced_param(&init.expr, replacements))
        }
        syn::Stmt::Item(_) => false,
        syn::Stmt::Expr(expr, _) => expr_mentions_replaced_param(expr, replacements),
        syn::Stmt::Macro(_) => true,
    }
}

/// Returns whether a pattern mentions a replaced parameter.
///
/// Patterns that the macro does not analyze, such as literals, ranges, and paths, are treated as
/// mentions.
fn pat_mentions_replaced_param(pat: &Pat, replacements: &[Replacement]) -> bool {
    match pat {
        Pat::Ident(pat) => pat
            .subpat
            .as_ref()
            .is_some_and(|(_at, subpat)| pat_mentions_replaced_param(subpat, replacements)),
        Pat::Or(pat) => pat
            .cases
            .iter()
            .any(|case| pat_mentions_replaced_param(case, replacements)),
        Pat::Paren(pat) => pat_mentions_replaced_param(&pat.pat, replacements),
        Pat::Reference(pat) => pat_mentions_replaced_param(&pat.pat, replacements),
        Pat::Rest(_) => false,
        Pat::Slice(pat) => pat
            .elems
            .iter()
            .any(|elem| pat_mentions_replaced_param(elem, replacements)),
        Pat::Struct(pat) => pat
            .fields
            .iter()
            .any(|field| pat_mentions_replaced_param(&field.pat, replacements)),
        Pat::Tuple(pat) => pat
            .elems
            .iter()
            .any(|elem| pat_mentions_replaced_param(elem, replacements)),
        Pat::TupleStruct(pat) => pat
            .elems
            .iter()
            .any(|elem| pat_mentions_replaced_param(elem, replacements)),
        Pat::Type(pat) => {
            pat_mentions_replaced_param(&pat.pat, replacements)
                || type_mentions_replaced_param(&pat.ty, replacements)
        }
        Pat::Wild(_) => false,
        _ => true,
    }
}

/// Returns whether a path in type position mentions a replaced parameter.
///
/// A path that starts with a replaced type parameter matches, as does a path with a generic
/// argument that mentions a replaced parameter.
fn type_path_mentions_replaced_param(path: &Path, replacements: &[Replacement]) -> bool {
    let directly_mentions_replaced_param = path.segments.first().is_some_and(|segment| {
        replacement_for(&segment.ident, replacements)
            .is_some_and(|replacement| replacement.kind == ReplacementKind::Type)
    });

    directly_mentions_replaced_param
        || path
            .segments
            .iter()
            .any(|segment| path_arguments_mention_replaced_param(&segment.arguments, replacements))
}

/// Returns whether a path in expression position mentions a replaced parameter.
///
/// A path that starts with any replaced parameter matches, as does a path with a generic argument
/// that mentions a replaced parameter.
fn expr_path_mentions_replaced_param(path: &Path, replacements: &[Replacement]) -> bool {
    let directly_mentions_replaced_param = path.segments.first().is_some_and(|segment| {
        replacement_for(&segment.ident, replacements).is_some_and(|replacement| {
            replacement.kind == ReplacementKind::Const || replacement.kind == ReplacementKind::Type
        })
    });

    directly_mentions_replaced_param
        || path
            .segments
            .iter()
            .any(|segment| path_arguments_mention_replaced_param(&segment.arguments, replacements))
}

/// Returns whether the arguments of a path segment mention a replaced parameter.
///
/// Both angle-bracketed and parenthesized arguments are checked.
fn path_arguments_mention_replaced_param(
    arguments: &PathArguments,
    replacements: &[Replacement],
) -> bool {
    match arguments {
        PathArguments::None => false,
        PathArguments::AngleBracketed(args) => {
            angle_bracketed_args_mention_replaced_param(args, replacements)
        }
        PathArguments::Parenthesized(args) => {
            args.inputs
                .iter()
                .any(|ty| type_mentions_replaced_param(ty, replacements))
                || return_type_mentions_replaced_param(&args.output, replacements)
        }
    }
}

/// Returns whether angle-bracketed generic arguments mention a replaced parameter.
///
/// Each argument is checked independently.
fn angle_bracketed_args_mention_replaced_param(
    args: &AngleBracketedGenericArguments,
    replacements: &[Replacement],
) -> bool {
    args.args
        .iter()
        .any(|arg| generic_argument_mentions_replaced_param(arg, replacements))
}

/// Returns whether a generic argument mentions a replaced parameter.
///
/// Associated type, associated const, and constraint arguments are checked together with their
/// own generic arguments.
fn generic_argument_mentions_replaced_param(
    arg: &GenericArgument,
    replacements: &[Replacement],
) -> bool {
    match arg {
        GenericArgument::Lifetime(_) => false,
        GenericArgument::Type(ty) => type_mentions_replaced_param(ty, replacements),
        GenericArgument::Const(expr) => expr_mentions_replaced_param(expr, replacements),
        GenericArgument::AssocType(assoc) => {
            assoc
                .generics
                .as_ref()
                .is_some_and(|args| angle_bracketed_args_mention_replaced_param(args, replacements))
                || type_mentions_replaced_param(&assoc.ty, replacements)
        }
        GenericArgument::AssocConst(assoc) => {
            assoc
                .generics
                .as_ref()
                .is_some_and(|args| angle_bracketed_args_mention_replaced_param(args, replacements))
                || expr_mentions_replaced_param(&assoc.value, replacements)
        }
        GenericArgument::Constraint(constraint) => {
            constraint_mentions_replaced_param(constraint, replacements)
        }
        _ => false,
    }
}

/// Returns whether an associated type constraint mentions a replaced parameter.
///
/// The generic arguments and bounds of the constraint are checked.
fn constraint_mentions_replaced_param(
    constraint: &Constraint,
    replacements: &[Replacement],
) -> bool {
    constraint
        .generics
        .as_ref()
        .is_some_and(|args| angle_bracketed_args_mention_replaced_param(args, replacements))
        || constraint
            .bounds
            .iter()
            .any(|bound| type_param_bound_mentions_replaced_param(bound, replacements))
}
