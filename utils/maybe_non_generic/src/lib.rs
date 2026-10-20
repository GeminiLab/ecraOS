use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream, Parser};
use syn::visit::{self, Visit};
use syn::visit_mut::{self, VisitMut};
use syn::{
    AngleBracketedGenericArguments, Attribute, Block, Constraint, Expr, ExprCall, ExprMethodCall,
    FnArg, GenericArgument, GenericParam, Generics, Ident, ImplItemFn, ItemFn, Pat, Path,
    PathArguments, PredicateLifetime, PredicateType, Result, ReturnType, Signature, Token,
    TraitBound, Type, TypeParamBound, WherePredicate, parenthesized, parse_quote,
    punctuated::Punctuated, token::Comma, token::Plus,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementKind {
    Const,
    Type,
}

#[derive(Debug)]
struct Replacement {
    kind: ReplacementKind,
    param: Ident,
    arg_name: Ident,
    arg_type: Type,
}

#[derive(Debug)]
enum FnTarget {
    Path(Path),
    Method { receiver: Expr, method: Ident },
}

#[derive(Debug)]
struct FnReplacement {
    from: FnTarget,
    to: FnTarget,
}

#[derive(Debug)]
struct MacroArgs {
    output: Ident,
    replacements: Vec<Replacement>,
    fn_replacements: Vec<FnReplacement>,
    copy_attrs: bool,
    extra_attrs: Vec<Attribute>,
}

#[derive(Debug)]
enum MacroArg {
    Replacement(Replacement),
    FnReplacement(FnReplacement),
    DontCopyAttr,
    Attr(Attribute),
}

#[proc_macro_attribute]
pub fn maybe_non_generic(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    maybe_non_generic_impl(attrs.into(), input.into())
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

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

fn should_copy_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "maybe_non_generic")
}

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

impl Parse for MacroArgs {
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

impl Parse for MacroArg {
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

    if let Ok(Expr::Field(field)) = syn::parse2::<Expr>(target.clone())
        && let syn::Member::Named(method) = field.member
    {
        return Ok(FnTarget::Method {
            receiver: *field.base,
            method,
        });
    }

    syn::parse2::<Path>(target).map(FnTarget::Path)
}

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

fn replacement_for<'a>(ident: &Ident, replacements: &'a [Replacement]) -> Option<&'a Replacement> {
    replacements
        .iter()
        .find(|replacement| replacement.param == *ident)
}

fn replacement_for_generic_param<'a>(
    param: &GenericParam,
    replacements: &'a [Replacement],
) -> Option<&'a Replacement> {
    replacements
        .iter()
        .find(|replacement| generic_param_matches_replacement(param, replacement))
}

fn generic_param_ident(param: &GenericParam) -> &Ident {
    match param {
        GenericParam::Lifetime(param) => &param.lifetime.ident,
        GenericParam::Type(param) => &param.ident,
        GenericParam::Const(param) => &param.ident,
    }
}

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

fn generic_param_matches_replacement(param: &GenericParam, replacement: &Replacement) -> bool {
    match (replacement.kind, param) {
        (ReplacementKind::Const, GenericParam::Const(param)) => param.ident == replacement.param,
        (ReplacementKind::Type, GenericParam::Type(param)) => param.ident == replacement.param,
        _ => false,
    }
}

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

fn path_segment_idents(path: &Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

fn paths_match_ignoring_turbofish(actual: &Path, expected: &Path) -> bool {
    path_segment_idents(actual) == path_segment_idents(expected)
}

fn method_receiver_matches(actual: &Expr, expected: &Expr) -> bool {
    actual.to_token_stream().to_string() == expected.to_token_stream().to_string()
}

fn is_type_ident(ty: &Type, ident: &Ident) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };

    type_path.qself.is_none()
        && type_path.path.segments.len() == 1
        && type_path.path.segments[0].ident == *ident
}

fn expr_is_const_ident(expr: &Expr, ident: &Ident) -> bool {
    let Expr::Path(expr_path) = expr else {
        return false;
    };

    expr_path.qself.is_none()
        && expr_path.path.segments.len() == 1
        && expr_path.path.segments[0].ident == *ident
}

fn replacement_expr(replacement: &Replacement) -> Expr {
    let arg_name = &replacement.arg_name;
    match replacement.kind {
        ReplacementKind::Const => parse_quote!(#arg_name),
        ReplacementKind::Type => parse_quote!(Clone::clone(&#arg_name)),
    }
}

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

fn residual_replaced_param_error(
    block: &Block,
    replacements: &[Replacement],
) -> Option<syn::Error> {
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

struct BodyTransformer<'a> {
    replacements: &'a [Replacement],
    fn_replacements: &'a [FnReplacement],
    errors: Vec<syn::Error>,
}

impl<'a> BodyTransformer<'a> {
    fn new(args: &'a MacroArgs) -> Self {
        Self {
            replacements: &args.replacements,
            fn_replacements: &args.fn_replacements,
            errors: Vec::new(),
        }
    }

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

impl VisitMut for BodyTransformer<'_> {
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

    fn visit_expr_method_call_mut(&mut self, node: &mut ExprMethodCall) {
        self.visit_expr_mut(&mut node.receiver);
        for arg in &mut node.args {
            self.visit_expr_mut(arg);
        }
        self.rewrite_method_call(node);
    }

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
}

fn transform_body(block: &mut Block, args: &MacroArgs) -> Result<()> {
    BodyTransformer::new(args).transform_block(block)
}

struct BindingConflictVisitor<'a> {
    replacements: &'a [Replacement],
    error: Option<syn::Error>,
}

impl<'a> BindingConflictVisitor<'a> {
    fn check(block: &'a Block, replacements: &'a [Replacement]) -> Option<syn::Error> {
        let mut visitor = Self {
            replacements,
            error: None,
        };
        visitor.visit_block(block);
        visitor.error
    }

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

impl<'ast> Visit<'ast> for BindingConflictVisitor<'ast> {
    fn visit_pat(&mut self, node: &'ast Pat) {
        self.check_pat(node);
        visit::visit_pat(self, node);
    }
}

fn body_binding_conflict_error(block: &Block, replacements: &[Replacement]) -> Option<syn::Error> {
    BindingConflictVisitor::check(block, replacements)
}

fn conflicting_pat_binding<'a>(pat: &'a Pat, replacements: &[Replacement]) -> Option<&'a Ident> {
    match pat {
        Pat::Ident(pat) => replacements
            .iter()
            .any(|replacement| replacement.arg_name == pat.ident)
            .then_some(&pat.ident),
        _ => None,
    }
}

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
        Pat::Rest(_) => {}
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
        Pat::Wild(_) => {}
        _ => {}
    }
}

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

fn predicate_mentions_replaced_param(
    predicate: &WherePredicate,
    replacements: &[Replacement],
) -> bool {
    match predicate {
        WherePredicate::Lifetime(predicate) => {
            lifetime_predicate_mentions_replaced_param(predicate, replacements)
        }
        WherePredicate::Type(predicate) => {
            type_predicate_mentions_replaced_param(predicate, replacements)
        }
        _ => false,
    }
}

fn lifetime_predicate_mentions_replaced_param(
    _predicate: &PredicateLifetime,
    _replacements: &[Replacement],
) -> bool {
    false
}

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

fn type_param_bound_mentions_replaced_param(
    bound: &TypeParamBound,
    replacements: &[Replacement],
) -> bool {
    match bound {
        TypeParamBound::Trait(bound) => trait_bound_mentions_replaced_param(bound, replacements),
        TypeParamBound::Lifetime(_) => false,
        TypeParamBound::Verbatim(_) => true,
        _ => true,
    }
}

fn trait_bound_mentions_replaced_param(bound: &TraitBound, replacements: &[Replacement]) -> bool {
    type_path_mentions_replaced_param(&bound.path, replacements)
}

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
        Type::Verbatim(_) => true,
        _ => true,
    }
}

fn return_type_mentions_replaced_param(
    return_type: &ReturnType,
    replacements: &[Replacement],
) -> bool {
    match return_type {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => type_mentions_replaced_param(ty, replacements),
    }
}

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
        Expr::Verbatim(_) => true,
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

fn block_mentions_replaced_param(block: &syn::Block, replacements: &[Replacement]) -> bool {
    block
        .stmts
        .iter()
        .any(|stmt| stmt_mentions_replaced_param(stmt, replacements))
}

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

fn angle_bracketed_args_mention_replaced_param(
    args: &AngleBracketedGenericArguments,
    replacements: &[Replacement],
) -> bool {
    args.args
        .iter()
        .any(|arg| generic_argument_mentions_replaced_param(arg, replacements))
}

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

#[cfg(test)]
mod tests;
