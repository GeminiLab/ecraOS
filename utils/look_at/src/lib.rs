#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Error, FnArg, GenericParam, Ident, Pat, Path, Result, Signature, Type, Visibility,
    parse_macro_input, parse_quote, spanned::Spanned,
};

use crate::input::{LookAtInput, LookAtItem, LookAtItemKind, TypeParamBounds};

/// Macro input syntax.
///
/// Defines the parsed invocation, its forwarding declarations, and their parsing rules.
mod input;

/// Collects identifiers declared by a function parameter pattern.
///
/// The identifiers form a readable forwarding name when a pattern has multiple bindings.
fn collect_pattern_identifiers(pattern: &Pat, identifiers: &mut Vec<Ident>) {
    match pattern {
        Pat::Ident(pattern) => {
            identifiers.push(pattern.ident.clone());
            if let Some((_, pattern)) = &pattern.subpat {
                collect_pattern_identifiers(pattern, identifiers);
            }
        }
        Pat::Or(pattern) => {
            if let Some(case) = pattern.cases.first() {
                collect_pattern_identifiers(case, identifiers);
            }
        }
        Pat::Paren(pattern) => collect_pattern_identifiers(&pattern.pat, identifiers),
        Pat::Reference(pattern) => collect_pattern_identifiers(&pattern.pat, identifiers),
        Pat::Slice(pattern) => {
            for element in &pattern.elems {
                collect_pattern_identifiers(element, identifiers);
            }
        }
        Pat::Struct(pattern) => {
            for field in &pattern.fields {
                collect_pattern_identifiers(&field.pat, identifiers);
            }
        }
        Pat::Tuple(pattern) => {
            for element in &pattern.elems {
                collect_pattern_identifiers(element, identifiers);
            }
        }
        Pat::TupleStruct(pattern) => {
            for element in &pattern.elems {
                collect_pattern_identifiers(element, identifiers);
            }
        }
        Pat::Type(pattern) => collect_pattern_identifiers(&pattern.pat, identifiers),
        _ => {}
    }
}

/// Chooses a readable forwarding name for a function parameter pattern.
///
/// Existing bindings are preserved or combined, with an ordinal fallback for nameless patterns.
fn forwarding_parameter_name(pattern: &Pat, index: usize, used_names: &mut Vec<String>) -> Ident {
    let mut identifiers = Vec::new();
    collect_pattern_identifiers(pattern, &mut identifiers);
    let candidate = match identifiers.as_slice() {
        [identifier] => identifier.clone(),
        [] => format_ident!("argument_{}", index + 1, span = pattern.span()),
        identifiers => {
            let name = identifiers
                .iter()
                .map(|identifier| identifier.to_string().trim_start_matches("r#").to_owned())
                .collect::<Vec<_>>()
                .join("_and_");
            format_ident!("{name}", span = pattern.span())
        }
    };
    let canonical_name = candidate.to_string().trim_start_matches("r#").to_owned();
    let candidate = if used_names.contains(&canonical_name) {
        format_ident!(
            "{}_argument_{}",
            canonical_name,
            index + 1,
            span = pattern.span()
        )
    } else {
        candidate
    };
    used_names.push(candidate.to_string().trim_start_matches("r#").to_owned());
    candidate
}

/// Expands one forwarding function.
///
/// The wrapper preserves the declared parameter names and checks the target function signature.
fn expand_function(
    target_path: &Path,
    attrs: Vec<Attribute>,
    vis: Visibility,
    mut sig: Signature,
) -> Result<TokenStream2> {
    if let Some(asyncness) = &sig.asyncness {
        return Err(Error::new(
            asyncness.span(),
            "`look_at` does not support async functions",
        ));
    }
    if let Some(variadic) = &sig.variadic {
        return Err(Error::new(
            variadic.span(),
            "`look_at` does not support variadic functions",
        ));
    }

    let mut call_args = Vec::new();
    let mut input_types = Vec::new();
    let mut used_names = Vec::new();
    for (index, argument) in sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = argument else {
            return Err(Error::new(
                argument.span(),
                "`look_at` only supports free functions",
            ));
        };
        let ident = forwarding_parameter_name(&argument.pat, index, &mut used_names);
        call_args.push(ident.clone());
        input_types.push(argument.ty.clone());
        *argument.pat = parse_quote!(#ident);
    }

    let name = &sig.ident;
    let generic_args = sig
        .generics
        .params
        .iter()
        .filter_map(|parameter| match parameter {
            GenericParam::Type(parameter) => {
                let ident = &parameter.ident;
                Some(quote!(#ident))
            }
            GenericParam::Const(parameter) => {
                let ident = &parameter.ident;
                Some(quote!(#ident))
            }
            GenericParam::Lifetime(_) => None,
        })
        .collect::<Vec<_>>();
    let target = if generic_args.is_empty() {
        quote!(#target_path::#name)
    } else {
        quote!(#target_path::#name::<#(#generic_args),*>)
    };

    let contains_impl_trait = input_types
        .iter()
        .any(|ty| matches!(ty.as_ref(), Type::ImplTrait(_)))
        || matches!(
            &sig.output,
            syn::ReturnType::Type(_, ty) if matches!(ty.as_ref(), Type::ImplTrait(_))
        );
    let type_check = if sig.constness.is_none() && !contains_impl_trait {
        let unsafety = &sig.unsafety;
        let abi = &sig.abi;
        let output = &sig.output;
        quote! { let _: #unsafety #abi fn(#(#input_types),*) #output = #target; }
    } else {
        TokenStream2::new()
    };
    let call = quote!(#target(#(#call_args),*));
    let call = if sig.unsafety.is_some() {
        quote!(unsafe { #call })
    } else {
        call
    };
    let sig = &sig;
    Ok(quote! { #(#attrs)* #[inline(always)] #vis #sig { #type_check #call } })
}

/// Expands one constant declaration.
///
/// The generated constant has the declared type and takes its value from the target constant, so
/// the compiler checks the target constant against the declared type.
fn expand_const(
    target_path: &Path,
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    declared_type: Type,
) -> Result<TokenStream2> {
    Ok(quote! { #(#attrs)* #vis const #name: #declared_type = #target_path::#name; })
}

/// Expands one use tree declaration.
///
/// The tree is prefixed with the target path, so nested groups and renames resolve inside the
/// selected module.
fn expand_use(
    target_path: &Path,
    attrs: Vec<Attribute>,
    vis: Visibility,
    tree: syn::UseTree,
) -> Result<TokenStream2> {
    Ok(quote! { #(#attrs)* #vis use #target_path::#tree; })
}

/// Expands one type declaration with optional trait bounds.
///
/// The type is re-exported from the target module. Bounds produce an anonymous constant that
/// checks the target type, and that check keeps only the conditional attributes of the declaration.
fn expand_type(
    target_path: &Path,
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    bounds: Option<TypeParamBounds>,
) -> Result<TokenStream2> {
    let reexport = quote! { #(#attrs)* #vis use #target_path::#name; };
    let conditional_attrs = attrs
        .iter()
        .filter(|attribute| {
            attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
        })
        .collect::<Vec<_>>();
    let assertion = bounds.map(|bounds| {
        quote! {
            #(#conditional_attrs)*
            const _: () = {
                let _: fn() = || {
                    fn assert_impl<T: #bounds>() {}
                    let _ = assert_impl::<#target_path::#name>;
                };
            };
        }
    });
    Ok(quote! { #reexport #assertion })
}

/// Expands one placeholder item.
///
/// The item's attributes and visibility are passed to the expansion for its declaration kind.
fn expand_item(item: LookAtItem, target_path: &Path) -> Result<TokenStream2> {
    let attrs = item.attrs;
    let vis = item.vis;

    match item.kind {
        LookAtItemKind::Function(function) => expand_function(target_path, attrs, vis, function),
        LookAtItemKind::Const {
            name,
            declared_type,
        } => expand_const(target_path, attrs, vis, name, declared_type),
        LookAtItemKind::Use(tree) => expand_use(target_path, attrs, vis, tree),
        LookAtItemKind::Type { name, bounds } => expand_type(target_path, attrs, vis, name, bounds),
    }
}

/// Expands all declarations in one macro invocation.
///
/// Every declaration resolves relative to the same selected implementation module.
fn expand(input: LookAtInput) -> Result<TokenStream2> {
    let target_path = input.target_path;
    input
        .items
        .into_iter()
        .map(|item| expand_item(item, &target_path))
        .collect()
}

/// Generates wrappers and re-exports for declarations in an implementation module.
///
/// `look_at!` collects the public interface shared by several architecture-specific modules. The
/// invocation names one implementation module, then lists declarations that should be available at
/// the invocation site. The declarations are intentionally only signatures or placeholders: the
/// implementation remains in the selected module.
///
/// # Overview
///
/// The target path is resolved once and is used for every item in the invocation. Functions are
/// emitted as wrappers that call the target function. Types and `use` trees are re-exported, while
/// constants are emitted as typed forwarding constants. The macro does not select the
/// implementation module. A conditional import, such as the `#[cfg]` imports of `current` below,
/// still selects it, and `look_at!` states and checks the interface that the selected module must
/// provide.
///
/// ```
/// # use look_at::look_at;
/// mod x86_64 {
///     pub fn halt() {}
///     pub const PAGE_SIZE: usize = 4096;
/// }
///
/// mod riscv64 {
///     pub fn halt() {}
///     pub const PAGE_SIZE: usize = 4096;
/// }
///
/// #[cfg(target_arch = "x86_64")]
/// use x86_64 as current;
/// #[cfg(target_arch = "riscv64")]
/// use riscv64 as current;
///
/// look_at! {
///     @current:
///     pub fn halt();
///     pub const PAGE_SIZE: usize;
/// }
/// ```
///
/// The generated `halt` function calls `x86_64::halt` and `PAGE_SIZE` has the value from
/// `x86_64::PAGE_SIZE` when building for x86_64, and their `riscv64` counterparts when building for
/// riscv64.
///
/// # Syntax
///
/// An invocation starts with `@`, followed by a Rust path and `:`. Every following declaration
/// must end with `;`. Attributes and visibility precede the declaration just as they do on ordinary
/// Rust items.
///
/// ```ignore
/// look_at! {
///     @crate::arch::current:
///     #[cfg(feature = "debug")]
///     pub fn initialize();
///     pub type Context: Send;
///     pub type Handle;
///     pub const PAGE_SIZE: usize;
///     pub use Error as InitError;
/// }
/// ```
///
/// The invocation must contain at least one item. A function declaration has no body, and all
/// other declarations are placeholders without a definition or value.
///
/// ## Functions
///
/// Function declarations keep their name, visibility, generics, `where` clause, ABI, and `const`
/// and `unsafe` qualifiers. Their parameter patterns are replaced with readable local names so the
/// generated wrapper can forward each argument positionally. A single binding keeps its name,
/// several bindings are joined with `_and_`, and a pattern without bindings becomes `argument_N`
/// for its one-based position. A repeated name receives an `_argument_N` suffix. Wrappers are
/// marked `#[inline(always)]`.
///
/// ```ignore
/// look_at! {
///     @crate::arch::current:
///     pub fn copy_bytes<'a>(source: &'a [u8], destination: &mut [u8]);
///     pub unsafe extern "C" fn enter(stack: *mut u8) -> !;
///     pub fn combine<T>(left: T, right: T) -> T where T: Add<Output = T>;
/// }
/// ```
///
/// Ordinary functions receive a generated function-pointer assignment that checks the declared
/// signature against the selected target. Functions containing `impl Trait` in an input or output
/// remain callable wrappers, but skip that generated function-pointer check because opaque types
/// cannot appear in it. `const` functions also skip it, and their wrappers can only call `const`
/// targets. The forwarding call still checks the arguments and the return value of these
/// functions, but it permits coercions.
///
/// An `unsafe` wrapper calls its target in an `unsafe` block, so callers take on the safety
/// requirements of the declaration. `async` functions, variadic functions, functions with a `self`
/// receiver, and declarations with a body are rejected.
///
/// ## Type declarations
///
/// A `type` declaration is a placeholder for a target type alias, struct, enum, or union. It emits
/// a `use` re-export, so the target's complete type definition and generic interface remain intact.
/// Generic parameters are accepted to describe the intended interface, but are not independently
/// checked or redeclared by the macro.
///
/// ```ignore
/// look_at! {
///     @crate::arch::current:
///     pub type Device;
///     pub type Buffer<T>;
///     pub type Context: Send + Sync;
/// }
/// ```
///
/// A bound is allowed only on a non-generic placeholder. The macro checks it inside an anonymous
/// constant, for example `pub type Context: Send;` verifies that the selected `Context` implements
/// `Send` at compile time. Without a bound, the declaration only checks that the target type
/// exists.
///
/// ## Constants and use trees
///
/// A constant placeholder contains the name and expected type, but no value. The expansion reads
/// the value from the selected module and assigns it to the declared type. A target constant of
/// another type is rejected unless its value coerces to the declared type.
///
/// ```ignore
/// look_at! {
///     @crate::arch::current:
///     pub const MAX_CPUS: usize;
///     pub use scheduler::Task as ArchTask;
/// }
/// ```
///
/// A `use` declaration is resolved below the target path, so aliases and nested use trees can be
/// forwarded without repeating the architecture path at each use site. It only checks that the
/// named items exist.
///
/// # Compared with conditional re-exports
///
/// The conditional import in the overview example already keeps target selection in one place, so
/// the selected items could also be re-exported directly:
///
/// ```
/// # mod x86_64 {
/// #     pub fn halt() {}
/// # }
/// # mod riscv64 {
/// #     pub fn halt() {}
/// # }
/// #[cfg(target_arch = "x86_64")]
/// use x86_64 as current;
/// #[cfg(target_arch = "riscv64")]
/// use riscv64 as current;
/// # #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
/// # use x86_64 as current;
///
/// pub use current::halt;
/// # fn main() {}
/// ```
///
/// Both forms give callers one set of names without their own `#[cfg]` attributes, and both
/// forward only the listed items. `look_at!` differs in how the interface is checked, which
/// visibility its items can have, and what it generates. The remaining examples in this section
/// define `current` directly and omit the `#[cfg]` selection.
///
/// ## Interface checks
///
/// A re-export exposes whatever the selected module defines. When one implementation diverges from
/// the intended interface, the crate that owns the interface still compiles. The mismatch surfaces
/// only where a caller uses the item on that target, or not at all when nothing uses it. If the
/// interface requires `page_shift` to return `u32`, this re-export of an implementation that
/// returns `u64` still compiles:
///
/// ```
/// mod current {
///     pub fn page_shift() -> u64 {
///         12
///     }
/// }
///
/// pub use current::page_shift;
/// # fn main() {}
/// ```
///
/// `look_at!` checks each declaration against the selected module at the invocation site.
///
/// - An ordinary function is assigned to a function pointer with the declared signature, so its
///   parameter types, return type, and ABI must match exactly.
/// - A constant is assigned to the declared type.
/// - A type placeholder with bounds asserts that the target type implements them.
///
/// Declaring the same interface with `look_at!` rejects that implementation with a type mismatch:
///
/// ```compile_fail,E0308
/// # use look_at::look_at;
/// mod current {
///     pub fn page_shift() -> u64 {
///         12
///     }
/// }
///
/// look_at! {
///     @current:
///     pub fn page_shift() -> u32;
/// }
/// # fn main() {}
/// ```
///
/// As with a re-export, a declaration also fails to compile when the selected module lacks the
/// item. The checks cover only the module selected for the current build, so each implementation
/// is checked when building for its own target.
///
/// ## Visibility
///
/// A re-export cannot be more visible than its target, so `pub use` rejects a crate-private
/// function:
///
/// ```compile_fail,E0364
/// mod current {
///     pub(crate) fn halt() {}
/// }
///
/// pub use current::halt;
/// # fn main() {}
/// ```
///
/// Function wrappers and forwarding constants are new items, and their targets only need to be
/// accessible from the invocation site. An implementation module can therefore keep its items
/// crate-private while the declarations form the public interface:
///
/// ```
/// # use look_at::look_at;
/// mod current {
///     pub(crate) fn halt() {}
///     pub(crate) const PAGE_SIZE: usize = 4096;
/// }
///
/// look_at! {
///     @current:
///     pub fn halt();
///     pub const PAGE_SIZE: usize;
/// }
/// # fn main() {}
/// ```
///
/// Type and `use` declarations expand to re-exports and follow the ordinary re-export rules.
///
/// ## Costs
///
/// - A function wrapper is a distinct function. Its address differs from the target's, and
///   attributes on the target, such as `#[must_use]` or `#[track_caller]`, do not apply to the
///   wrapper unless the declaration repeats them.
/// - Each signature is written once more in the declaration and changes together with the
///   implementations.
/// - Some declarations are checked only partially. Type placeholders without bounds and `use`
///   declarations only check that the item exists, and generic parameters on type placeholders
///   are not checked. `const` functions and functions with `impl Trait` skip the function-pointer
///   check and rely on the forwarding call, which permits coercions.
/// - Some items cannot be declared, as listed in [Unsupported items](#unsupported-items).
///
/// An attribute that callers of the wrapper should observe is therefore written on the declaration
/// as well as on the implementation:
///
/// ```
/// # use look_at::look_at;
/// mod current {
///     #[must_use]
///     pub fn read_counter() -> u64 {
///         0
///     }
/// }
///
/// look_at! {
///     @current:
///     #[must_use]
///     pub fn read_counter() -> u64;
/// }
/// # fn main() {}
/// ```
///
/// A conditional re-export remains the simpler choice when callers need the target item itself,
/// such as its function address or its attributes, when the items cannot be declared here, or when
/// the interface needs no checks beyond those its callers already perform.
///
/// # Attributes and visibility
///
/// Outer attributes attached to an item, including doc comments, are copied to its generated
/// wrapper, re-export, or constant. In particular, `#[cfg(...)]` can remove an item for
/// architectures where it is unavailable. A bound check copies only the `cfg` and `cfg_attr`
/// attributes of its declaration, so it is removed together with its re-export.
///
/// The visibility written in the invocation is used for the generated item and does not increase
/// the visibility of the target definition itself. Function wrappers and constants may be more
/// visible than their targets, while type and `use` re-exports may not, as described in
/// [Visibility](#visibility).
///
/// # Documentation
///
/// Function wrappers and forwarding constants are new items, so their documentation consists only
/// of the doc comments on the declaration. Type and `use` declarations expand to re-exports. When
/// rustdoc inlines such a re-export, for example because the target module is private, it shows
/// the doc comments on the declaration followed by those on the target item. That part of the page
/// therefore depends on the target selected for the documentation build.
///
/// Doc comments on a declaration are shared by every implementation. They describe one interface
/// regardless of the target, but they must hold for every architecture and cannot document the
/// details of one implementation. A practical split is to describe the architecture-neutral
/// contract on the declaration and to keep architecture-specific details on each implementation:
///
/// ```
/// # use look_at::look_at;
/// mod x86_64 {
///     /// Returns 12 because x86-64 uses 4 KiB base pages.
///     pub fn page_shift() -> u32 {
///         12
///     }
/// }
///
/// use x86_64 as current;
///
/// look_at! {
///     @current:
///     /// Returns the base-2 logarithm of the base page size.
///     pub fn page_shift() -> u32;
/// }
/// # fn main() {}
/// ```
///
/// The documentation of the generated `page_shift` contains only the declaration's sentence, while
/// the x86-64 detail stays with the implementation.
///
/// # Unsupported items
///
/// The current syntax supports functions, `type` placeholders, `const` placeholders, and `use`
/// trees. Trait declarations, structs with inline fields, enum definitions, union definitions, and
/// constants with values must remain in the selected implementation module and are not accepted as
/// declarations here. Their definitions can still be forwarded with a `type`, `const`, or `use`
/// declaration. Among functions, `async` functions, variadic functions, and functions with a
/// `self` receiver are rejected, as described in [Functions](#functions).
#[proc_macro]
pub fn look_at(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as LookAtInput);
    expand(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Unit tests for macro parsing and expansion.
///
/// These tests inspect tokens and diagnostics without depending on compiler stderr formatting.
#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the expansion error for one complete macro input.
    ///
    /// The input must parse successfully so the message specifically tests semantic validation.
    fn expansion_error(input: &str) -> String {
        let input = syn::parse_str::<LookAtInput>(input).expect("macro input should parse");
        expand(input)
            .expect_err("expansion should be rejected")
            .to_string()
    }

    /// Returns the parsing error for one complete macro input.
    ///
    /// The input must be structurally invalid and rejected before expansion.
    fn parsing_error(input: &str) -> String {
        match syn::parse_str::<LookAtInput>(input) {
            Ok(_) => panic!("macro input should be rejected while parsing"),
            Err(error) => error.to_string(),
        }
    }

    /// Verifies parsing of mixed function and type declarations.
    ///
    /// A single invocation may contain multiple supported item kinds.
    #[test]
    fn parses_functional_input() {
        let input =
            syn::parse_str::<LookAtInput>("@crate::arch: pub fn run(value: u32); pub type Value;")
                .expect("input should parse");
        assert_eq!(input.items.len(), 2);
    }

    /// Verifies generation of nominal type bound checks.
    ///
    /// The assertion references the selected target type inside an anonymous constant.
    #[test]
    fn expands_bound_assertion() {
        let input = syn::parse_str::<LookAtInput>("@crate::arch: pub type Value: Send;")
            .expect("input should parse");
        let output = expand(input).expect("input should expand").to_string();
        assert!(output.contains("assert_impl"));
        assert!(output.contains("crate :: arch :: Value"));
    }

    /// Verifies generation of a constant forwarding declaration.
    ///
    /// The generated constant assigns the selected target value to the declared type.
    #[test]
    fn expands_constant_forwarding() {
        let input = syn::parse_str::<LookAtInput>("@crate::arch: pub const VALUE: u32;")
            .expect("input should parse");
        let output = expand(input).expect("input should expand").to_string();
        assert!(output.contains("const VALUE : u32 = crate :: arch :: VALUE"));
    }

    /// Verifies readable forwarding names for nontrivial parameter patterns.
    ///
    /// Source identifiers are combined and internal underscore-prefixed names are never emitted.
    #[test]
    fn expands_readable_parameter_names() {
        let input = syn::parse_str::<LookAtInput>(
            "@crate::arch: fn tuple((left, right): (u32, u32)); \
             fn borrow(ref value: String); \
             fn collision((first, second): (u32, u32), first_and_second: u32);",
        )
        .expect("input should parse");
        let output = expand(input).expect("input should expand").to_string();
        assert!(output.contains("left_and_right : (u32 , u32)"));
        assert!(output.contains("value : String"));
        assert!(output.contains("first_and_second_argument_2 : u32"));
        assert!(!output.contains("__at_arg"));
    }

    /// Verifies that asynchronous functions are rejected explicitly.
    ///
    /// Async wrappers require forwarding semantics outside this macro's current scope.
    #[test]
    fn rejects_async_functions() {
        assert_eq!(
            expansion_error("@crate::arch: async fn run();"),
            "`look_at` does not support async functions"
        );
    }

    /// Verifies that variadic functions are rejected explicitly.
    ///
    /// Rust cannot transparently forward a C variadic argument list.
    #[test]
    fn rejects_variadic_functions() {
        assert_eq!(
            expansion_error("@crate::arch: unsafe extern \"C\" fn log(level: u32, ...);"),
            "`look_at` does not support variadic functions"
        );
    }

    /// Verifies that function declarations cannot contain bodies.
    ///
    /// A body would conflict with the forwarding body generated by the macro.
    #[test]
    fn rejects_function_bodies() {
        assert_eq!(
            parsing_error("@crate::arch: fn run() {}"),
            "functions in `look_at` must not have a body"
        );
    }

    /// Verifies that trait declarations remain unsupported.
    ///
    /// Trait forwarding is intentionally outside the current item set.
    #[test]
    fn rejects_trait_items() {
        assert_eq!(
            parsing_error("@crate::arch: pub trait Service;"),
            "expected a function, type, const, or use"
        );
    }

    /// Verifies that bounds are rejected on generic type placeholders.
    ///
    /// Generic parameters are accepted for re-exports, but the current syntax does not combine
    /// them with a trait-bound assertion.
    #[test]
    fn rejects_bounds_on_generic_types() {
        assert_eq!(
            parsing_error("@crate::arch: pub type Value<T>: Send;"),
            "trait bounds are not supported on generic items"
        );
    }

    /// Verifies parsing of all non-function declaration forms.
    ///
    /// Type, constant, and use placeholders can be mixed with function declarations.
    #[test]
    fn parses_non_function_items() {
        let input = syn::parse_str::<LookAtInput>(
            "@crate::arch: pub type Value; pub type GenericValue<T>; pub const VALUE: u32; \
             pub use Value as ReexportedValue;",
        )
        .expect("input should parse");
        assert_eq!(input.items.len(), 4);
    }

    /// Verifies that an invocation cannot omit all forwarding declarations.
    ///
    /// A target path without an item would otherwise expand to no output and hide a configuration
    /// mistake.
    #[test]
    fn rejects_empty_input() {
        assert_eq!(
            parsing_error("@crate::arch:"),
            "unexpected end of input, `look_at` requires at least one item"
        );
    }
}
