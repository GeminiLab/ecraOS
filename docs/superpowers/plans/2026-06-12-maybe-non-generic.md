# maybe_non_generic Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `maybe_non_generic` attribute proc macro, use it to generate the non-generic `expt` helper copies, and restore the `expt` test suite.

**Architecture:** Build a new `maybe_non_generic` proc-macro crate modeled after `dyn_static_traits`, but with explicit parser, signature transformer, and body AST visitor units. The macro keeps the original function and appends a generated copy whose selected generics become ordinary runtime parameters. `expt` then switches its commented macro annotations to the new syntax and deletes the equivalent hand-written copies.

**Tech Stack:** Rust 2024, `proc_macro`, `syn` with `full` and `visit-mut`, `quote`, `proc-macro2`, `cargo check -p expt`, `cargo test -p expt`.

---

## File Structure

- Create `maybe_non_generic/Cargo.toml`: proc-macro crate manifest.
- Create `maybe_non_generic/src/lib.rs`: macro parser, AST transformation logic, proc-macro entrypoint, and focused unit tests.
- Modify `Cargo.toml`: add `maybe_non_generic` to workspace members.
- Modify `Cargo.lock`: updated by Cargo after adding the crate.
- Modify `expt/Cargo.toml`: add path dependency on `maybe_non_generic`.
- Modify `expt/src/lib.rs`: import the macro, replace commented annotations with active new syntax, remove hand-written copies.
- Modify `expt/src/tests.rs`: remove stale generic arguments from `cursor()` calls after macro integration passes.

Keep the first implementation in one `maybe_non_generic/src/lib.rs` file. Split later only if the file becomes difficult to review; the local proc-macro precedent in `dyn_static_traits/src/lib.rs` is a single focused file.

## Task 1: Scaffold the proc-macro crate

**Files:**
- Create: `maybe_non_generic/Cargo.toml`
- Create: `maybe_non_generic/src/lib.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the workspace member**

Edit the root `Cargo.toml` workspace members list so it includes `"maybe_non_generic"`. Keep existing members unchanged.

```toml
[workspace]
resolver = "3"
members = [
    "ecraos",
    "explat/explat",
    "explat/explat-x86_64",
    "ecraos-loader",
    "exboot/exboot",
    "exboot/exboot-macros",
    "exboot/exboot-multiboot-x86_64", "exbuddy", "exslab", "expt", "size-disp", "memory_range_set", "expercpu/expercpu", "expercpu/expercpu_macros"
, "dyn_static_traits", "maybe_non_generic"]
```

- [ ] **Step 2: Create the crate manifest**

Create `maybe_non_generic/Cargo.toml`:

```toml
[package]
name = "maybe_non_generic"
version = "0.1.0"
edition = "2024"

[lib]
proc-macro = true

[dependencies]
proc-macro2 = "1.0"
quote = "1.0"
syn = { version = "2.0", features = ["full", "visit-mut", "extra-traits"] }
```

- [ ] **Step 3: Create a compiling macro stub**

Create `maybe_non_generic/src/lib.rs`:

```rust
use proc_macro::TokenStream as TokenStream1;

#[proc_macro_attribute]
pub fn maybe_non_generic(_attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    input
}
```

- [ ] **Step 4: Verify the new crate compiles**

Run:

```sh
cargo check -p maybe_non_generic
```

Expected: PASS. Cargo may update `Cargo.lock` to include the new local crate.

- [ ] **Step 5: Commit the scaffold**

```sh
git add Cargo.toml Cargo.lock maybe_non_generic/Cargo.toml maybe_non_generic/src/lib.rs
git commit -m "feat: add maybe_non_generic crate"
```

## Task 2: Implement and test macro argument parsing

**Files:**
- Modify: `maybe_non_generic/src/lib.rs`

- [ ] **Step 1: Replace the stub with parser types and tests**

Replace `maybe_non_generic/src/lib.rs` with this parser-focused implementation:

```rust
use proc_macro::TokenStream as TokenStream1;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parenthesized,
    parse::{Parse, ParseStream, Parser},
    Attribute, Expr, Ident, Path, Result, Token, Type,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementKind {
    Const,
    Type,
}

#[derive(Clone, Debug)]
struct Replacement {
    kind: ReplacementKind,
    param: Ident,
    arg_name: Ident,
    arg_type: Type,
}

#[derive(Clone, Debug)]
enum FnTarget {
    Path(Path),
    Method { receiver: Expr, method: Ident },
}

#[derive(Clone, Debug)]
struct FnReplacement {
    from: FnTarget,
    to: FnTarget,
}

#[derive(Clone, Debug)]
struct MacroArgs {
    output_name: Ident,
    replacements: Vec<Replacement>,
    fn_replacements: Vec<FnReplacement>,
    copy_attrs: bool,
    extra_attrs: Vec<Attribute>,
}

enum MacroArg {
    Replacement(Replacement),
    FnReplacement(FnReplacement),
    DontCopyAttr,
    Attr(Attribute),
}

impl Parse for MacroArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let output_name: Ident = input.parse()?;
        let mut replacements = Vec::new();
        let mut fn_replacements = Vec::new();
        let mut copy_attrs = true;
        let mut extra_attrs = Vec::new();

        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }

        while !input.is_empty() {
            match input.parse::<MacroArg>()? {
                MacroArg::Replacement(replacement) => replacements.push(replacement),
                MacroArg::FnReplacement(replacement) => fn_replacements.push(replacement),
                MacroArg::DontCopyAttr => copy_attrs = false,
                MacroArg::Attr(attr) => extra_attrs.push(attr),
            }

            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        validate_args(&replacements)?;

        Ok(Self {
            output_name,
            replacements,
            fn_replacements,
            copy_attrs,
            extra_attrs,
        })
    }
}

impl Parse for MacroArg {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name: Ident = input.parse()?;
        let name_text = name.to_string();

        match name_text.as_str() {
            "const" => parse_replacement(input, ReplacementKind::Const).map(MacroArg::Replacement),
            "type" => parse_replacement(input, ReplacementKind::Type).map(MacroArg::Replacement),
            "fn" => parse_fn_replacement(input).map(MacroArg::FnReplacement),
            "dont_copy_attr" => {
                if input.peek(syn::token::Paren) {
                    return Err(syn::Error::new(name.span(), "dont_copy_attr does not take parentheses"));
                }
                Ok(MacroArg::DontCopyAttr)
            }
            "attr" => parse_attr(input).map(MacroArg::Attr),
            _ => Err(syn::Error::new(name.span(), "unknown maybe_non_generic argument")),
        }
    }
}

fn parse_replacement(input: ParseStream<'_>, kind: ReplacementKind) -> Result<Replacement> {
    let content;
    parenthesized!(content in input);
    let param: Ident = content.parse()?;
    content.parse::<Token![=>]>()?;
    let arg_name: Ident = content.parse()?;
    content.parse::<Token![:]>()?;
    let arg_type: Type = content.parse()?;
    if !content.is_empty() {
        return Err(content.error("each const(...) or type(...) accepts exactly one replacement"));
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
        return Err(content.error("fn(...) accepts exactly one path replacement"));
    }
    Ok(FnReplacement { from, to })
}

fn parse_fn_target(input: ParseStream<'_>) -> Result<FnTarget> {
    let expr: Expr = input.parse()?;
    match expr {
        Expr::Path(expr_path) => Ok(FnTarget::Path(expr_path.path)),
        Expr::Field(expr_field) => {
            let syn::Member::Named(method) = expr_field.member else {
                return Err(syn::Error::new_spanned(expr_field.member, "method replacement must name a method"));
            };
            Ok(FnTarget::Method {
                receiver: *expr_field.base,
                method,
            })
        }
        other => Err(syn::Error::new_spanned(
            other,
            "function replacement target must be a path or receiver.method",
        )),
    }
}

fn parse_attr(input: ParseStream<'_>) -> Result<Attribute> {
    let content;
    let paren = parenthesized!(content in input);
    let tokens: TokenStream = content.parse()?;
    if tokens.is_empty() {
        return Err(syn::Error::new(paren.span.open(), "attr(...) must not be empty"));
    }
    let attrs = Attribute::parse_outer.parse2(quote!(#[#tokens]))?;
    attrs
        .into_iter()
        .next()
        .ok_or_else(|| syn::Error::new(paren.span.open(), "attr(...) must parse as an attribute"))
}

fn validate_args(replacements: &[Replacement]) -> Result<()> {
    for (index, replacement) in replacements.iter().enumerate() {
        for previous in &replacements[..index] {
            if replacement.param == previous.param {
                return Err(syn::Error::new(
                    replacement.param.span(),
                    "duplicate replacement for generic parameter",
                ));
            }
            if replacement.arg_name == previous.arg_name {
                return Err(syn::Error::new(
                    replacement.arg_name.span(),
                    "duplicate generated argument name",
                ));
            }
        }
    }
    Ok(())
}

#[proc_macro_attribute]
pub fn maybe_non_generic(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    let _attrs = syn::parse::<MacroArgs>(attrs).unwrap();
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::ToTokens;
    use syn::parse_str;

    #[test]
    fn parses_all_argument_kinds() {
        let args: MacroArgs = parse_str(
            "copy, const(LEVEL => level: usize), type(H => handler: DynPagingHandler), \
             fn(self.clear_pte => self.clear_pte_dyn), \
             fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn), \
             dont_copy_attr, attr(allow(dead_code))",
        )
        .unwrap();

        assert_eq!(args.output_name, "copy");
        assert_eq!(args.replacements.len(), 2);
        assert_eq!(args.replacements[0].kind, ReplacementKind::Const);
        assert_eq!(args.replacements[0].param, "LEVEL");
        assert_eq!(args.replacements[0].arg_name, "level");
        assert_eq!(args.replacements[1].kind, ReplacementKind::Type);
        assert_eq!(args.replacements[1].param, "H");
        assert_eq!(args.fn_replacements.len(), 2);
        assert!(!args.copy_attrs);
        assert_eq!(args.extra_attrs.len(), 1);
        assert_eq!(
            args.extra_attrs[0].to_token_stream().to_string(),
            "# [allow (dead_code)]"
        );
    }

    #[test]
    fn rejects_grouped_const_rules() {
        let err = parse_str::<MacroArgs>("copy, const(A => a: usize, B => b: usize)")
            .unwrap_err()
            .to_string();
        assert!(err.contains("exactly one replacement"));
    }

    #[test]
    fn rejects_duplicate_replacement_param() {
        let err = parse_str::<MacroArgs>("copy, type(H => handler: Dyn), type(H => other: Dyn)")
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate replacement"));
    }
}
```

- [ ] **Step 2: Run parser tests**

Run:

```sh
cargo test -p maybe_non_generic
```

Expected: PASS with 3 unit tests.

- [ ] **Step 3: Commit parser implementation**

```sh
git add maybe_non_generic/src/lib.rs
git commit -m "feat(maybe_non_generic): parse macro arguments"
```

## Task 3: Add signature generation

**Files:**
- Modify: `maybe_non_generic/src/lib.rs`

- [ ] **Step 1: Add target and signature transformation helpers**

In `maybe_non_generic/src/lib.rs`, extend imports:

```rust
use quote::{quote, ToTokens};
use syn::{
    parse_quote, punctuated::Punctuated, token::Comma, FnArg, GenericParam, Generics, ImplItemFn,
    ItemFn, Pat, PatIdent, PredicateType, ReturnType, Signature, TypeParamBound, WherePredicate,
};
```

Add these helpers before the proc-macro entrypoint:

```rust
fn replacement_for<'a>(ident: &Ident, replacements: &'a [Replacement]) -> Option<&'a Replacement> {
    replacements.iter().find(|replacement| replacement.param == *ident)
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
        let Some(param) = sig
            .generics
            .params
            .iter()
            .find(|param| generic_param_ident(param) == &replacement.param)
        else {
            return Err(syn::Error::new(
                replacement.param.span(),
                "replacement generic parameter does not exist on the function",
            ));
        };

        match (replacement.kind, param) {
            (ReplacementKind::Const, GenericParam::Const(_)) => {}
            (ReplacementKind::Type, GenericParam::Type(_)) => {}
            (ReplacementKind::Const, _) => {
                return Err(syn::Error::new(replacement.param.span(), "const(...) must name a const generic parameter"));
            }
            (ReplacementKind::Type, _) => {
                return Err(syn::Error::new(replacement.param.span(), "type(...) must name a type generic parameter"));
            }
        }
    }

    Ok(())
}

fn signature_for_copy(original: &Signature, args: &MacroArgs) -> Result<Signature> {
    validate_replacements_against_signature(original, args)?;

    let mut sig = original.clone();
    sig.ident = args.output_name.clone();
    sig.generics = generics_for_copy(&sig.generics, &args.replacements);

    for replacement in &args.replacements {
        let arg_name = &replacement.arg_name;
        let arg_type = &replacement.arg_type;
        let pat: Pat = parse_quote!(#arg_name);
        sig.inputs.push(FnArg::Typed(parse_quote!(#pat: #arg_type)));
    }

    Ok(sig)
}

fn generics_for_copy(generics: &Generics, replacements: &[Replacement]) -> Generics {
    let mut result = generics.clone();
    result.params = generics
        .params
        .iter()
        .filter(|param| replacement_for(generic_param_ident(param), replacements).is_none())
        .cloned()
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

fn predicate_mentions_replaced_param(predicate: &WherePredicate, replacements: &[Replacement]) -> bool {
    let text = predicate.to_token_stream().to_string();
    replacements
        .iter()
        .any(|replacement| text.split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|part| part == replacement.param.to_string()))
}
```

- [ ] **Step 2: Add signature tests**

Append these tests to the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn removes_replaced_generics_and_appends_args() {
        let args: MacroArgs = parse_str(
            "table_of_mut_non_const_dyn, const(LEVEL => level: usize), type(H => handler: DynPagingHandler)",
        )
        .unwrap();
        let item: ImplItemFn = parse_quote! {
            fn table_of_mut<'a, const LEVEL: usize, H: PagingHandler>(paddr: PhysAddr) -> &'a mut [PTE]
            where
                [(); LEVEL]: Sized,
                [(); M::LEVELS]: Sized,
            {
                unreachable!()
            }
        };

        let sig = signature_for_copy(&item.sig, &args).unwrap();

        assert_eq!(
            sig.to_token_stream().to_string(),
            "fn table_of_mut_non_const_dyn < 'a > (paddr : PhysAddr , level : usize , handler : DynPagingHandler) -> & 'a mut [PTE] where [(); M :: LEVELS] : Sized"
        );
    }

    #[test]
    fn rejects_wrong_replacement_kind() {
        let args: MacroArgs = parse_str("copy, const(H => handler: usize)").unwrap();
        let item: ItemFn = parse_quote! {
            fn original<H>() {}
        };

        let err = signature_for_copy(&item.sig, &args).unwrap_err().to_string();
        assert!(err.contains("const(...) must name a const generic parameter"));
    }
```

- [ ] **Step 3: Run signature tests**

Run:

```sh
cargo test -p maybe_non_generic removes_replaced_generics_and_appends_args rejects_wrong_replacement_kind
```

Expected: PASS.

- [ ] **Step 4: Commit signature transformation**

```sh
git add maybe_non_generic/src/lib.rs
git commit -m "feat(maybe_non_generic): generate copy signatures"
```

## Task 4: Implement function body AST transformation

**Files:**
- Modify: `maybe_non_generic/src/lib.rs`

- [ ] **Step 1: Add body visitor imports**

Extend imports in `maybe_non_generic/src/lib.rs`:

```rust
use syn::{
    AngleBracketedGenericArguments, Block, ExprCall, ExprMethodCall, GenericArgument,
    PathArguments,
};
use syn::visit_mut::{self, VisitMut};
```

- [ ] **Step 2: Add path matching and generic argument helpers**

Add these helpers before the proc-macro entrypoint:

```rust
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
    parse_quote!(#arg_name)
}

fn remove_replaced_generic_args(
    args: &mut AngleBracketedGenericArguments,
    replacements: &[Replacement],
) -> Vec<Expr> {
    let mut appended = Vec::new();
    let mut kept = Punctuated::<GenericArgument, Comma>::new();

    for arg in args.args.iter().cloned() {
        let replacement = match &arg {
            GenericArgument::Type(ty) => replacements
                .iter()
                .find(|replacement| replacement.kind == ReplacementKind::Type && is_type_ident(ty, &replacement.param)),
            GenericArgument::Const(expr) => replacements
                .iter()
                .find(|replacement| replacement.kind == ReplacementKind::Const && expr_is_const_ident(expr, &replacement.param)),
            _ => None,
        };

        if let Some(replacement) = replacement {
            appended.push(replacement_expr(replacement));
        } else {
            kept.push(arg);
        }
    }

    args.args = kept;
    appended
}

fn process_path_generic_args(path: &mut Path, replacements: &[Replacement]) -> Vec<Expr> {
    let mut appended = Vec::new();
    for segment in &mut path.segments {
        if let PathArguments::AngleBracketed(args) = &mut segment.arguments {
            appended.extend(remove_replaced_generic_args(args, replacements));
            if args.args.is_empty() {
                segment.arguments = PathArguments::None;
            }
        }
    }
    appended
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
```

- [ ] **Step 3: Add the AST visitor**

Add this visitor before the proc-macro entrypoint:

```rust
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
        self.visit_block_mut(block);
        if self.errors.is_empty() {
            Ok(())
        } else {
            let mut iter = self.errors.into_iter();
            let mut error = iter.next().unwrap();
            for next in iter {
                error.combine(next);
            }
            Err(error)
        }
    }

    fn type_replacement_for_path(&self, path: &Path) -> Option<(&Replacement, Ident)> {
        if path.leading_colon.is_some() || path.segments.len() != 2 {
            return None;
        }

        let mut segments = path.segments.iter();
        let first = segments.next().unwrap();
        let second = segments.next().unwrap();

        if !matches!(first.arguments, PathArguments::None) {
            return None;
        }

        self.replacements
            .iter()
            .find(|replacement| replacement.kind == ReplacementKind::Type && replacement.param == first.ident)
            .map(|replacement| (replacement, second.ident.clone()))
    }

    fn rewrite_method_call(&mut self, node: &mut ExprMethodCall) {
        for mapping in self.fn_replacements {
            let (FnTarget::Method { receiver, method }, FnTarget::Method { method: to_method, .. }) =
                (&mapping.from, &mapping.to)
            else {
                continue;
            };

            if node.method == *method && method_receiver_matches(&node.receiver, receiver) {
                node.method = to_method.clone();
            }
        }

        if let Some(turbofish) = &mut node.turbofish {
            let appended = remove_replaced_generic_args(turbofish, self.replacements);
            if turbofish.args.is_empty() {
                node.turbofish = None;
            }
            node.args.extend(appended);
        }
    }
}

impl VisitMut for BodyTransformer<'_> {
    fn visit_expr_call_mut(&mut self, node: &mut ExprCall) {
        for arg in &mut node.args {
            self.visit_expr_mut(arg);
        }

        if let Expr::Path(expr_path) = &mut *node.func {
            if let Some((replacement, item)) = self.type_replacement_for_path(&expr_path.path) {
                let arg_name = &replacement.arg_name;
                node.func = parse_quote!((#arg_name.#item));
                return;
            }

            if let Err(error) = rewrite_path_call(&mut expr_path.path, self.fn_replacements) {
                self.errors.push(error);
                return;
            }

            let appended = process_path_generic_args(&mut expr_path.path, self.replacements);
            node.args.extend(appended);
        } else {
            self.visit_expr_mut(&mut node.func);
        }
    }

    fn visit_expr_method_call_mut(&mut self, node: &mut ExprMethodCall) {
        visit_mut::visit_expr_method_call_mut(self, node);
        self.rewrite_method_call(node);
    }

    fn visit_expr_mut(&mut self, node: &mut Expr) {
        match node {
            Expr::Call(call) => self.visit_expr_call_mut(call),
            Expr::MethodCall(call) => self.visit_expr_method_call_mut(call),
            Expr::Path(expr_path) => {
                if expr_path.qself.is_none() && expr_path.path.segments.len() == 1 {
                    let ident = &expr_path.path.segments[0].ident;
                    if let Some(replacement) = self
                        .replacements
                        .iter()
                        .find(|replacement| replacement.kind == ReplacementKind::Const && replacement.param == *ident)
                    {
                        let arg_name = &replacement.arg_name;
                        *node = parse_quote!(#arg_name);
                        return;
                    }
                }

                if let Some((replacement, item)) = self.type_replacement_for_path(&expr_path.path) {
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
```

- [ ] **Step 4: Add body transformation tests**

Append these tests:

```rust
    fn transformed_block(input: syn::Block, args: &str) -> String {
        let args: MacroArgs = parse_str(args).unwrap();
        let mut block = input;
        transform_body(&mut block, &args).unwrap();
        block.to_token_stream().to_string()
    }

    #[test]
    fn rewrites_type_static_function_call_to_function_pointer_field() {
        let output = transformed_block(
            parse_quote!({
                let paddr = H::alloc_page_aligned(bytes_required);
                let vaddr = H::phys_to_virt(paddr);
            }),
            "copy, type(H => handler: DynPagingHandler)",
        );

        assert!(output.contains("(handler . alloc_page_aligned) (bytes_required)"));
        assert!(output.contains("(handler . phys_to_virt) (paddr)"));
    }

    #[test]
    fn rewrites_const_expression_paths() {
        let output = transformed_block(
            parse_quote!({
                let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];
                let child = LEVEL - 1;
            }),
            "copy, const(LEVEL => level: usize)",
        );

        assert!(output.contains("M :: LEVEL_TABLE_SIZE [level]"));
        assert!(output.contains("level - 1"));
    }

    #[test]
    fn rewrites_path_call_and_removes_replaced_turbofish_args() {
        let output = transformed_block(
            parse_quote!({
                let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
            }),
            "copy, type(H => handler: DynPagingHandler), fn(Self::alloc_table => Self::alloc_table_dyn)",
        );

        assert!(output.contains("Self :: alloc_table_dyn :: < { M :: LEVELS - 1 } > (handler) ?"));
    }

    #[test]
    fn rewrites_method_call_only_with_explicit_mapping() {
        let output = transformed_block(
            parse_quote!({
                self.clear_pte::<H>(child, level - 1, child_vaddr)?;
            }),
            "copy, type(H => handler: DynPagingHandler), fn(self.clear_pte => self.clear_pte_dyn)",
        );

        assert!(output.contains("self . clear_pte_dyn (child , level - 1 , child_vaddr , handler) ?"));
    }
```

- [ ] **Step 5: Run body transformation tests**

Run:

```sh
cargo test -p maybe_non_generic rewrites_
```

Expected: PASS for the four `rewrites_*` tests.

- [ ] **Step 6: Commit body transformation**

```sh
git add maybe_non_generic/src/lib.rs
git commit -m "feat(maybe_non_generic): transform function bodies"
```

## Task 5: Connect macro expansion and attribute handling

**Files:**
- Modify: `maybe_non_generic/src/lib.rs`

- [ ] **Step 1: Add expansion helpers**

Add these helpers before the proc-macro entrypoint:

```rust
fn should_copy_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .map(|segment| segment.ident != "maybe_non_generic")
        .unwrap_or(true)
}

fn attrs_for_copy(original: &[Attribute], args: &MacroArgs) -> Vec<Attribute> {
    let mut attrs = Vec::new();
    if args.copy_attrs {
        attrs.extend(original.iter().filter(|attr| should_copy_attr(attr)).cloned());
    }
    attrs.extend(args.extra_attrs.iter().cloned());
    attrs
}

fn expand_item_fn(args: &MacroArgs, item: ItemFn) -> Result<TokenStream> {
    let mut generated = item.clone();
    generated.attrs = attrs_for_copy(&item.attrs, args);
    generated.sig = signature_for_copy(&item.sig, args)?;
    transform_body(&mut generated.block, args)?;

    Ok(quote! {
        #item
        #generated
    })
}

fn expand_impl_item_fn(args: &MacroArgs, item: ImplItemFn) -> Result<TokenStream> {
    let mut generated = item.clone();
    generated.attrs = attrs_for_copy(&item.attrs, args);
    generated.sig = signature_for_copy(&item.sig, args)?;
    transform_body(&mut generated.block, args)?;

    Ok(quote! {
        #item
        #generated
    })
}

fn maybe_non_generic_impl(attrs: TokenStream, input: TokenStream) -> Result<TokenStream> {
    let args = syn::parse2::<MacroArgs>(attrs)?;
    let input2 = input;

    if let Ok(item) = syn::parse2::<ItemFn>(input2.clone()) {
        return expand_item_fn(&args, item);
    }

    if let Ok(item) = syn::parse2::<ImplItemFn>(input2.clone()) {
        return expand_impl_item_fn(&args, item);
    }

    Err(syn::Error::new_spanned(
        input2,
        "maybe_non_generic can only be applied to functions with bodies",
    ))
}
```

- [ ] **Step 2: Replace the proc-macro entrypoint**

Replace the existing `maybe_non_generic` function with:

```rust
#[proc_macro_attribute]
pub fn maybe_non_generic(attrs: TokenStream1, input: TokenStream1) -> TokenStream1 {
    maybe_non_generic_impl(attrs.into(), input.into())
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}
```

- [ ] **Step 3: Add expansion tests**

Append these tests:

```rust
    fn expand_for_test(attrs: &str, item: TokenStream) -> String {
        let attrs = attrs.parse::<TokenStream>().unwrap();
        maybe_non_generic_impl(attrs, item).unwrap().to_string()
    }

    #[test]
    fn expands_free_function_and_copies_doc_attr() {
        let output = expand_for_test(
            "copy, type(H => handler: DynHandler), attr(allow(dead_code))",
            quote! {
                /// docs
                fn original<H: Handler>() {
                    H::run();
                }
            },
        );

        assert!(output.contains("fn original"));
        assert!(output.contains("fn copy"));
        assert!(output.contains("# [doc = \" docs\"]"));
        assert!(output.contains("# [allow (dead_code)]"));
        assert!(output.contains("(handler . run) ()"));
    }

    #[test]
    fn dont_copy_attr_drops_original_attrs() {
        let output = expand_for_test(
            "copy, type(H => handler: DynHandler), dont_copy_attr, attr(allow(dead_code))",
            quote! {
                #[inline]
                fn original<H: Handler>() {
                    H::run();
                }
            },
        );

        assert!(!output.contains("# [inline]"));
        assert!(output.contains("# [allow (dead_code)]"));
    }
```

- [ ] **Step 4: Run all macro crate tests**

Run:

```sh
cargo test -p maybe_non_generic
cargo check -p maybe_non_generic
```

Expected: PASS.

- [ ] **Step 5: Commit proc-macro expansion**

```sh
git add maybe_non_generic/src/lib.rs
git commit -m "feat(maybe_non_generic): expand generated function copies"
```

## Task 6: Integrate the macro into expt

**Files:**
- Modify: `expt/Cargo.toml`
- Modify: `expt/src/lib.rs`
- Modify: `Cargo.lock`

- [ ] **Step 1: Add the dependency**

In `expt/Cargo.toml`, add the dependency:

```toml
maybe_non_generic = { path = "../maybe_non_generic" }
```

The `[dependencies]` section should include:

```toml
[dependencies]
cfg-if.workspace = true
dyn_static_traits = { path = "../dyn_static_traits" }
heapless = "0.9"
maybe_non_generic = { path = "../maybe_non_generic" }
memory_addr.workspace = true
page_table_entry = "0.6.1"
thiserror.workspace = true
```

- [ ] **Step 2: Import the macro**

In `expt/src/lib.rs`, add this near the existing imports:

```rust
use maybe_non_generic::maybe_non_generic;
```

- [ ] **Step 3: Activate `new_alloc_dyn` generation and remove the hand-written copy**

Replace:

```rust
    /// Allocates and initializes a new root page table through `H`.
    // #[maybe_non_generic(new_alloc_dyn; H => handler: DynPagingHandler; Self::alloc_table => Self::alloc_table_dyn)]
    pub fn new_alloc<H: PagingHandler>() -> PagingResult<Self>
    where
        [(); M::LEVELS - 1]: Sized,
    {
        let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
        Ok(unsafe { Self::new_at(paddr) })
    }

    pub fn new_alloc_dyn(handler: DynPagingHandler) -> PagingResult<Self>
    where
        [(); M::LEVELS - 1]: Sized,
    {
        let paddr = Self::alloc_table_dyn::<{ M::LEVELS - 1 }>(handler)?;
        Ok(unsafe { Self::new_at(paddr) })
    }
```

with:

```rust
    /// Allocates and initializes a new root page table through `H`.
    #[maybe_non_generic(
        new_alloc_dyn,
        type(H => handler: DynPagingHandler),
        fn(Self::alloc_table => Self::alloc_table_dyn)
    )]
    pub fn new_alloc<H: PagingHandler>() -> PagingResult<Self>
    where
        [(); M::LEVELS - 1]: Sized,
    {
        let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
        Ok(unsafe { Self::new_at(paddr) })
    }
```

- [ ] **Step 4: Activate `table_of_mut` copies and remove `table_of_mut_non_const`**

Replace the commented attributes and the hand-written `table_of_mut_non_const` with:

```rust
    /// Gets the table at level `LEVEL` from its physical address `paddr`.
    #[maybe_non_generic(
        table_of_mut_non_const,
        const(LEVEL => level: usize)
    )]
    #[maybe_non_generic(
        table_of_mut_dyn,
        type(H => handler: DynPagingHandler)
    )]
    #[maybe_non_generic(
        table_of_mut_non_const_dyn,
        const(LEVEL => level: usize),
        type(H => handler: DynPagingHandler)
    )]
    fn table_of_mut<'a, const LEVEL: usize, H: PagingHandler>(paddr: PhysAddr) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];

        unsafe {
            let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }
```

- [ ] **Step 5: Activate `alloc_table_dyn` generation and remove the hand-written copy**

Replace:

```rust
    // #[maybe_non_generic(alloc_table_dyn; H => handler: DynPagingHandler)]
    fn alloc_table<const LEVEL: usize, H: PagingHandler>() -> PagingResult<PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = H::alloc_page_aligned(bytes_required) {
            let vaddr = H::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }

    fn alloc_table_dyn<const LEVEL: usize>(handler: DynPagingHandler) -> PagingResult<PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = (handler.alloc_page_aligned)(bytes_required) {
            let vaddr = (handler.phys_to_virt)(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }
```

with:

```rust
    #[maybe_non_generic(
        alloc_table_dyn,
        type(H => handler: DynPagingHandler)
    )]
    fn alloc_table<const LEVEL: usize, H: PagingHandler>() -> PagingResult<PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = H::alloc_page_aligned(bytes_required) {
            let vaddr = H::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }
```

- [ ] **Step 6: Activate `clear_pte_dyn` generation**

Replace the commented `clear_pte` attribute with:

```rust
    #[maybe_non_generic(
        clear_pte_dyn,
        type(H => handler: DynPagingHandler),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(PageTable::table_of_mut_non_const => PageTable::table_of_mut_non_const_dyn)
    )]
```

Keep the original `clear_pte` body unchanged.

- [ ] **Step 7: Check expt**

Run:

```sh
cargo check -p expt
```

Expected: PASS. If it fails inside generated code, inspect the error span and add the missing AST form to `maybe_non_generic/src/lib.rs` with a focused unit test before changing `expt`.

- [ ] **Step 8: Commit expt integration**

```sh
git add Cargo.lock expt/Cargo.toml expt/src/lib.rs
git commit -m "refactor(expt): generate dynamic page-table helpers"
```

## Task 7: Fix the existing cursor test API mismatch

**Files:**
- Modify: `expt/src/tests.rs`

- [ ] **Step 1: Update stale cursor calls**

In `expt/src/tests.rs`, replace every:

```rust
table.cursor::<TestPagingHandler>()
```

with:

```rust
table.cursor()
```

There are currently 14 occurrences.

- [ ] **Step 2: Run the expt test suite**

Run:

```sh
cargo test -p expt
```

Expected: PASS. Existing warnings in `expt/src/opaque.rs` about unused closure arguments may remain.

- [ ] **Step 3: Commit the test cleanup**

```sh
git add expt/src/tests.rs
git commit -m "test(expt): update cursor calls"
```

## Task 8: Final verification

**Files:**
- Review: `maybe_non_generic/src/lib.rs`
- Review: `expt/src/lib.rs`
- Review: `expt/src/tests.rs`
- Review: `Cargo.toml`
- Review: `Cargo.lock`

- [ ] **Step 1: Run all required checks**

Run:

```sh
cargo test -p maybe_non_generic
cargo check -p maybe_non_generic
cargo check -p expt
cargo test -p expt
```

Expected: all commands PASS.

- [ ] **Step 2: Inspect generated integration surface**

Run:

```sh
rg -n "maybe_non_generic|new_alloc_dyn|table_of_mut_non_const_dyn|alloc_table_dyn|clear_pte_dyn|cursor::<" expt/src maybe_non_generic/src -S
```

Expected:

- active `#[maybe_non_generic(...)]` attributes exist in `expt/src/lib.rs`;
- no hand-written duplicate `new_alloc_dyn`, `table_of_mut_non_const`, or `alloc_table_dyn` bodies remain;
- `clear_pte_dyn` appears only as a generated target name in attributes or generated-code references visible through compiler diagnostics, not as a hand-written body;
- no `cursor::<` calls remain in `expt/src/tests.rs`.

- [ ] **Step 3: Check git status**

Run:

```sh
git status --short
```

Expected: clean working tree after the task commits, or only intentionally uncommitted files if the user asked to keep something staged or unstaged.
