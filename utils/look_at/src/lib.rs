use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, FnArg, GenericParam, Ident, Pat, Path, Result, Signature, Token, Type,
    Visibility, braced, parse_macro_input, parse_quote,
};

/// Arguments accepted by the `look_at` attribute.
///
/// The first argument selects the implementation module. The optional `flatten` argument controls
/// whether a decorated module remains in the generated output.
struct LookAtArgs {
    target_path: Path,
    flatten: bool,
}

impl Parse for LookAtArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let target_path = input.parse()?;
        let flatten = if input.is_empty() {
            false
        } else {
            input.parse::<Token![,]>()?;
            let option = input.parse::<Ident>()?;
            if option != "flatten" {
                return Err(Error::new(option.span(), "expected `flatten`"));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
            if !input.is_empty() {
                return Err(input.error("unexpected attribute argument"));
            }

            true
        };

        Ok(Self {
            target_path,
            flatten,
        })
    }
}

/// A function declaration accepted by `look_at`.
///
/// Directly and indirectly attributed functions must end with a semicolon and have no body.
struct ForwardFunction {
    attrs: Vec<Attribute>,
    vis: Visibility,
    sig: Signature,
}

impl Parse for ForwardFunction {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let attrs = Attribute::parse_outer(input)?;
        let vis = input.parse()?;
        let sig = input.parse()?;
        if !input.peek(Token![;]) {
            return Err(input.error("functions decorated by `look_at` must not have a body"));
        }
        input.parse::<Token![;]>()?;

        Ok(Self { attrs, vis, sig })
    }
}

/// An inline module accepted by `look_at`.
///
/// Its contents are parsed as forwarding function declarations so Rust-invalid free function
/// declarations can be transformed before normal item validation.
struct ForwardModule {
    attrs: Vec<Attribute>,
    vis: Visibility,
    ident: Ident,
    functions: Vec<ForwardFunction>,
}

impl Parse for ForwardModule {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let attrs = Attribute::parse_outer(input)?;
        let vis = input.parse()?;
        input.parse::<Token![mod]>()?;
        let ident = input.parse()?;

        let content;
        braced!(content in input);
        let mut functions = Vec::new();
        while !content.is_empty() {
            functions.push(content.parse()?);
        }

        Ok(Self {
            attrs,
            vis,
            ident,
            functions,
        })
    }
}

/// An item supported by the `look_at` attribute.
///
/// The item is distinguished after parsing its outer attributes and visibility.
enum ForwardItem {
    Function(Box<ForwardFunction>),
    Module(ForwardModule),
}

impl Parse for ForwardItem {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.fork();
        Attribute::parse_outer(&lookahead)?;
        lookahead.parse::<Visibility>()?;

        if lookahead.peek(Token![mod]) {
            input.parse().map(Self::Module)
        } else {
            input.parse().map(Box::new).map(Self::Function)
        }
    }
}

/// Expands one forwarding function.
///
/// Parameter patterns are replaced with private identifiers so every accepted declaration can be
/// forwarded as a positional call.
fn expand_function(target_path: &Path, mut function: ForwardFunction) -> Result<TokenStream2> {
    if let Some(asyncness) = &function.sig.asyncness {
        return Err(Error::new(
            asyncness.span(),
            "`look_at` does not support async functions",
        ));
    }
    if let Some(variadic) = &function.sig.variadic {
        return Err(Error::new(
            variadic.span(),
            "`look_at` does not support variadic functions",
        ));
    }

    let mut call_args = Vec::new();
    let mut input_types = Vec::new();
    for argument in function.sig.inputs.iter_mut() {
        let FnArg::Typed(argument) = argument else {
            return Err(Error::new(
                argument.span(),
                "`look_at` only supports free functions",
            ));
        };
        let Pat::Ident(ident) = &*argument.pat else {
            return Err(Error::new(
                argument.pat.span(),
                "`look_at` does not support patterns in function arguments",
            ));
        };
        call_args.push(ident.clone());
        input_types.push(argument.ty.clone());
    }

    let name = &function.sig.ident;
    let generic_args = function
        .sig
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
            &function.sig.output,
            syn::ReturnType::Type(_, ty) if matches!(ty.as_ref(), Type::ImplTrait(_))
        );
    let type_check = if function.sig.constness.is_none() && !contains_impl_trait {
        let unsafety = &function.sig.unsafety;
        let abi = &function.sig.abi;
        let output = &function.sig.output;
        quote! {
            let _: #unsafety #abi fn(#(#input_types),*) #output = #target;
        }
    } else {
        TokenStream2::new()
    };

    let call = quote!(#target(#(#call_args),*));
    let call = if function.sig.unsafety.is_some() {
        quote!(unsafe { #call })
    } else {
        call
    };
    let attrs = &function.attrs;
    let vis = &function.vis;
    let sig = &function.sig;

    Ok(quote! {
        #(#attrs)*
        #[inline(always)]
        #vis #sig {
            #type_check
            #call
        }
    })
}

/// Expands an attributed item.
///
/// Modules either remain as modules containing generated wrappers or yield those wrappers directly
/// when `flatten` is present.
fn expand(args: LookAtArgs, item: ForwardItem) -> Result<TokenStream2> {
    match item {
        ForwardItem::Function(function) => {
            if args.flatten {
                return Err(Error::new(
                    function.sig.ident.span(),
                    "`flatten` is only valid when `look_at` is applied to a module",
                ));
            }

            expand_function(&args.target_path, *function)
        }
        ForwardItem::Module(module) => {
            let wrappers = module
                .functions
                .into_iter()
                .map(|function| expand_function(&args.target_path, function))
                .collect::<Result<Vec<_>>>()?;

            if args.flatten {
                let attrs = &module.attrs;
                let wrappers = wrappers
                    .into_iter()
                    .map(|wrapper| quote!(#(#attrs)* #wrapper));
                Ok(quote!(#(#wrappers)*))
            } else {
                let attrs = &module.attrs;
                let vis = &module.vis;
                let ident = &module.ident;
                Ok(quote! {
                    #(#attrs)*
                    #vis mod #ident {
                        #(#wrappers)*
                    }
                })
            }
        }
    }
}

/// Generates wrappers for architecture-specific functions.
///
/// The attribute accepts an implementation module path and, for modules only, an optional
/// `flatten` argument.
#[proc_macro_attribute]
pub fn look_at(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as LookAtArgs);
    let item = parse_macro_input!(item as ForwardItem);

    expand(args, item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Unit tests for rejected macro inputs.
///
/// These tests exercise expansion errors directly so diagnostics remain independent of compiler
/// stderr formatting.
#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the expansion error for one attribute application.
    ///
    /// Both snippets must parse successfully so the resulting message specifically tests semantic
    /// validation.
    fn expansion_error(args: &str, item: &str) -> String {
        let args = syn::parse_str::<LookAtArgs>(args).expect("attribute arguments should parse");
        let item = syn::parse_str::<ForwardItem>(item).expect("attributed item should parse");
        expand(args, item)
            .expect_err("expansion should be rejected")
            .to_string()
    }

    /// Returns the parsing error for one attributed item.
    ///
    /// The snippet must be rejected before expansion so structural errors are reported immediately.
    fn parsing_error(item: &str) -> String {
        match syn::parse_str::<ForwardItem>(item) {
            Ok(_) => panic!("attributed item should be rejected while parsing"),
            Err(error) => error.to_string(),
        }
    }

    /// Verifies that asynchronous functions are rejected explicitly.
    ///
    /// An async wrapper would need different signature-checking and call-generation semantics.
    #[test]
    fn rejects_async_functions() {
        assert_eq!(
            expansion_error("crate::arch", "async fn run();"),
            "`look_at` does not support async functions"
        );
    }

    /// Verifies that variadic functions are rejected explicitly.
    ///
    /// Rust cannot transparently forward a C variadic argument list.
    #[test]
    fn rejects_variadic_functions() {
        assert_eq!(
            expansion_error(
                "crate::arch",
                "unsafe extern \"C\" fn log(level: u32, ...);"
            ),
            "`look_at` does not support variadic functions"
        );
    }

    /// Verifies that directly attributed functions must be declarations.
    ///
    /// A function body must be rejected by the item parser rather than replaced during expansion.
    #[test]
    fn rejects_function_bodies_on_directly_attributed_functions_during_parsing() {
        assert_eq!(
            parsing_error("fn run() {}"),
            "functions decorated by `look_at` must not have a body"
        );
    }

    /// Verifies that indirectly attributed functions must be declarations.
    ///
    /// Functions nested in an attributed module use the same parser and rejection point.
    #[test]
    fn rejects_function_bodies_inside_modules_during_parsing() {
        assert_eq!(
            parsing_error("mod wrappers { fn run() {} }"),
            "functions decorated by `look_at` must not have a body"
        );
    }

    /// Verifies that `flatten` is limited to modules.
    ///
    /// Flattening a standalone function has no defined structural effect.
    #[test]
    fn rejects_flatten_on_functions() {
        assert_eq!(
            expansion_error("crate::arch, flatten", "fn run();"),
            "`flatten` is only valid when `look_at` is applied to a module"
        );
    }
}
