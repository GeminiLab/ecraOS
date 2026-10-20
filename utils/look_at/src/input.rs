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
    ///
    /// Every declaration in the invocation resolves below this path.
    pub target_path: Path,
    /// The forwarding declarations accepted by `look_at`.
    ///
    /// The declarations are expanded in input order and the list is never empty.
    pub items: Vec<LookAtItem>,
}

/// The parsing implementation for a complete invocation.
///
/// It reads the `@path:` header followed by forwarding declarations until the input ends.
impl Parse for LookAtInput {
    /// Parses the target path and every forwarding declaration.
    ///
    /// An invocation without declarations is rejected.
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
    ///
    /// They are copied to the generated wrapper, re-export, or constant.
    pub attrs: Vec<Attribute>,
    /// The visibility of the forwarded item.
    ///
    /// It applies to the generated item and does not affect the target definition.
    pub vis: Visibility,
    /// The kind of the forwarded item.
    ///
    /// The kind determines how the declaration is expanded.
    pub kind: LookAtItemKind,
}

/// The parsing implementation for one forwarding declaration.
///
/// It reads outer attributes and visibility before the declaration itself.
impl Parse for LookAtItem {
    /// Parses the attributes, visibility, and kind of one declaration.
    ///
    /// The declaration kind consumes the terminating semicolon.
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let attrs = Attribute::parse_outer(input)?;
        let vis = input.parse()?;
        let kind = input.parse()?;

        Ok(Self { attrs, vis, kind })
    }
}

/// The kinds of items supported by `look_at`.
///
/// Each kind describes a declaration without an implementation, which is resolved in the target
/// module.
#[derive(Clone)]
pub enum LookAtItemKind {
    /// A function declaration.
    ///
    /// Forwarded functions contain a signature followed by a semicolon and no body.
    Function(Signature),
    /// A const declaration.
    ///
    /// Forwarded consts contain a name and a type followed by a semicolon and no value.
    Const {
        /// The name of the forwarded constant.
        ///
        /// The same name is used for the generated constant and the target constant.
        name: Ident,
        /// The type declared for the forwarded constant.
        ///
        /// The target constant must have a value of this type.
        declared_type: Type,
    },
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
        /// The name of the forwarded type.
        ///
        /// The same name is used for the re-export and the target type.
        name: Ident,
        /// The trait bounds that the target type must satisfy.
        ///
        /// Bounds are checked at compile time and are absent for generic placeholders.
        bounds: Option<TypeParamBounds>,
    },
}

/// The parsing implementation for a declaration kind.
///
/// It selects the kind from the leading keywords without consuming them during lookahead.
impl Parse for LookAtItemKind {
    /// Parses one function, const, use, or type declaration.
    ///
    /// Function bodies, const values, and bounds on generic type placeholders are rejected.
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
