use syn::{
    Attribute, Ident, Path, Signature, Token, Type, TypeParamBound, UseTree, Visibility,
    parse::{Parse, ParseStream, Result},
};

/// Trait bounds attached to a forwarded type.
///
/// Bounds use the same plus-separated syntax as Rust type parameter bounds.
pub type TypeParamBounds = syn::punctuated::Punctuated<TypeParamBound, Token![+]>;

/// The input accepted by the [`look_at`](crate::look_at) macro.
///
/// The input contains one target module followed by one or more forwarding declarations.
#[derive(Clone)]
pub struct LookAtInput {
    /// The path to the target module containing the forwarded items.
    pub target_path: Path,
    /// The forwarding declarations accepted by `look_at`.
    pub items: Vec<LookAtItem>,
}

impl Parse for LookAtInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        input.parse::<Token![@]>()?;
        let target_path = input.parse()?;
        input.parse::<Token![:]>()?;

        let mut items = Vec::new();
        while !input.is_empty() {
            items.push(input.parse()?);
        }
        if items.is_empty() {
            return Err(input.error("`look_at` requires at least one item"));
        }
        Ok(Self { target_path, items })
    }
}

/// An item supported by `look_at`.
///
/// Functions receive forwarding bodies while other declarations become re-exports.
#[derive(Clone)]
pub struct LookAtItem {
    /// The attributes attached to the forwarded item.
    pub attrs: Vec<Attribute>,
    /// The visibility of the forwarded item.
    pub vis: Visibility,
    /// The kind of the forwarded item.
    pub kind: LookAtItemKind,
}

impl Parse for LookAtItem {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let attrs = Attribute::parse_outer(input)?;
        let vis = input.parse()?;
        let kind = input.parse()?;

        Ok(Self { attrs, vis, kind })
    }
}

/// The kinds of items supported by `look_at`.
#[derive(Clone)]
pub enum LookAtItemKind {
    /// A function declaration.
    ///
    /// Forwarded functions contain a signature followed by a semicolon and no body.
    Function(Signature),
    /// A const declaration.
    ///
    /// Forwarded consts contain a name and a type followed by a semicolon and no value.
    Const { name: Ident, declared_type: Type },
    /// A use statement.
    ///
    /// The use tree is resolved relative to the selected target module.
    Use(UseTree),
    /// A type declaration.
    ///
    /// Forwarded types contain a name and optional trait bounds followed by a semicolon and no
    /// definition. It can point to a type alias, struct, enum, or union. Generic parameters are
    /// accepted for re-exports, but cannot be combined with trait bounds.
    Type {
        name: Ident,
        bounds: Option<TypeParamBounds>,
    },
}

impl Parse for LookAtItemKind {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.fork();

        if lookahead.peek(Token![fn])
            || lookahead.peek(Token![async])
            || lookahead.peek(Token![unsafe])
            || lookahead.peek(Token![extern])
            || (lookahead.peek(Token![const]) && lookahead.peek2(Token![fn]))
        {
            // It's Self::Function
            let result = input.parse().map(Self::Function);
            if !input.peek(Token![;]) {
                return Err(input.error("functions in `look_at` must not have a body"));
            }
            input.parse::<Token![;]>()?;
            result
        } else if lookahead.peek(Token![const]) {
            // It's Self::Const
            input.parse::<Token![const]>()?;
            let name = input.parse()?;
            input.parse::<Token![:]>()?;
            let declared_type = input.parse()?;
            input.parse::<Token![;]>()?;
            Ok(Self::Const {
                name,
                declared_type,
            })
        } else if lookahead.peek(Token![use]) {
            // It's Self::Use
            input.parse::<Token![use]>()?;
            let tree = input.parse()?;
            input.parse::<Token![;]>()?;
            Ok(Self::Use(tree))
        } else if lookahead.peek(Token![type]) {
            // It's Self::Type
            input.parse::<Token![type]>()?;
            let name = input.parse()?;
            let has_generics = input.peek(Token![<]);
            if has_generics {
                let _: syn::Generics = input.parse()?;
            }
            let bounds = if input.peek(Token![:]) {
                if has_generics {
                    return Err(input.error("trait bounds are not supported on generic items"));
                }
                input.parse::<Token![:]>()?;
                Some(TypeParamBounds::parse_separated_nonempty(input)?)
            } else {
                None
            };
            input.parse::<Token![;]>()?;
            Ok(Self::Type { name, bounds })
        } else {
            Err(input.error("expected a function, type, const, or use"))
        }
    }
}
