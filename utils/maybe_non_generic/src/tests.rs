use quote::{ToTokens, quote};
use syn::{parse_str, parse2};

use super::*;

/// Verifies parsing of every argument kind.
///
/// One invocation combines replacements, renames, `dont_copy_attr`, and `attr(...)`.
#[test]
fn parses_all_argument_kinds() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize),
        type(H => handler: DynPagingHandler),
        fn(self.clear_pte => self.clear_pte_dyn),
        fn(PageTable::table_of_mut => PageTable::table_of_mut_dyn),
        dont_copy_attr,
        attr(allow(dead_code))
    })
    .unwrap();

    assert_eq!(args.output.to_string(), "copy");
    assert_eq!(args.replacements.len(), 2);
    assert!(matches!(args.replacements[0].kind, ReplacementKind::Const));
    assert_eq!(args.replacements[0].param.to_string(), "LEVEL");
    assert_eq!(args.replacements[0].arg_name.to_string(), "level");
    assert!(matches!(args.replacements[1].kind, ReplacementKind::Type));
    assert_eq!(args.replacements[1].param.to_string(), "H");
    assert_eq!(args.replacements[1].arg_name.to_string(), "handler");
    assert_eq!(args.fn_replacements.len(), 2);
    assert!(matches!(
        args.fn_replacements[0].from,
        FnTarget::Method { .. }
    ));
    assert!(matches!(args.fn_replacements[1].from, FnTarget::Path(_)));
    assert!(!args.copy_attrs);
    assert_eq!(args.extra_attrs.len(), 1);
    assert_eq!(
        args.extra_attrs[0].to_token_stream().to_string(),
        "# [allow (dead_code)]"
    );
}

/// Verifies that one `const(...)` argument cannot hold several replacements.
///
/// Each generic parameter needs its own argument.
#[test]
fn rejects_grouped_const_rules() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        const(A => a: usize, B => b: usize)
    })
    .unwrap_err();

    assert!(err.to_string().contains("exactly one replacement"));
}

/// Verifies that a generic parameter cannot be replaced twice.
///
/// The second replacement of `H` is rejected while parsing.
#[test]
fn rejects_duplicate_replacement_param() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        type(H => handler: DynPagingHandler),
        type(H => other_handler: DynPagingHandler)
    })
    .unwrap_err();

    assert!(err.to_string().contains("duplicate replacement"));
}

/// Verifies that two replacements cannot generate the same parameter name.
///
/// The copy would otherwise declare one parameter twice.
#[test]
fn rejects_duplicate_generated_argument_name() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        type(H => handler: Dyn),
        type(T => handler: Dyn)
    })
    .unwrap_err();

    assert!(err.to_string().contains("duplicate generated"));
}

/// Verifies that a type replacement keeps a lifetime with the same name.
///
/// Only the type parameter `T` is removed, so `'T` stays on the copy.
#[test]
fn accepts_type_replacement_when_lifetime_has_same_name() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<'T, T>() {}
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < 'T > (replacement : Dyn)"
    );
}

/// Verifies that a const replacement keeps a lifetime with the same name.
///
/// Only the const parameter `N` is removed, so `'N` stays on the copy.
#[test]
fn accepts_const_replacement_when_lifetime_has_same_name() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(N => n: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<'N, const N: usize>() {}
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < 'N > (n : usize)"
    );
}

/// Verifies the copied signature for const and type replacements.
///
/// Replaced generics and their predicates are removed, and the generated parameters are appended
/// in order.
#[test]
fn removes_replaced_generics_and_appends_args() {
    let args = parse2::<MacroArgs>(quote! {
        table_of_mut_non_const_dyn,
        const(LEVEL => level: usize),
        type(H => handler: DynPagingHandler)
    })
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
        "fn table_of_mut_non_const_dyn < 'a > (paddr : PhysAddr , level : usize , handler : DynPagingHandler) -> & 'a mut [PTE] where [() ; M :: LEVELS] : Sized"
    );
}

/// Verifies that replacements must follow the generic parameter order.
///
/// The order of the appended parameters would otherwise differ from the signature.
#[test]
fn rejects_replacement_order_that_differs_from_function_generics() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => value: usize),
        const(N => n: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const N: usize, T>() {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("must be listed in function generic parameter order"),
        "{err}"
    );
}

/// Verifies that existing parameter types cannot use a replaced type.
///
/// The copy would refer to a type parameter that it no longer declares.
#[test]
fn rejects_replaced_type_in_existing_parameter_type() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<T>(value: T) {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("function parameter type still references a replaced generic parameter"),
        "{err}"
    );
}

/// Verifies that existing parameter types cannot use a replaced const.
///
/// An array length that uses the const would refer to a removed parameter.
#[test]
fn rejects_replaced_const_in_existing_parameter_type() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize>(value: [usize; LEVEL]) {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("function parameter type still references a replaced generic parameter"),
        "{err}"
    );
}

/// Verifies that the return type cannot use a replaced type.
///
/// The copy would return a type parameter that it no longer declares.
#[test]
fn rejects_replaced_type_in_return_type() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<T>() -> T {
            unreachable!()
        }
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("return type still references a replaced generic parameter"),
        "{err}"
    );
}

/// Verifies that a generated parameter name cannot repeat an existing parameter.
///
/// The copy would otherwise declare the same name twice.
#[test]
fn rejects_generated_argument_name_conflicting_with_existing_parameter() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => paddr: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize>(paddr: PhysAddr) {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("generated argument name conflicts with an existing function parameter")
    );
}

/// Verifies that a generated parameter name cannot repeat a nested parameter binding.
///
/// Bindings inside tuple patterns are included in the check.
#[test]
fn rejects_generated_argument_name_conflicting_with_nested_parameter_binding() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize>((level, x): (usize, usize)) {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("generated argument name conflicts with an existing function parameter")
    );
}

/// Verifies that retained parameters lose bounds on replaced parameters.
///
/// The `Into<T>` bound is removed from `U`, while `Clone` is kept.
#[test]
fn removes_retained_type_param_bounds_that_mention_replaced_param() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<T, U: Into<T> + Clone>() {}
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < U : Clone > (replacement : Dyn)"
    );
}

/// Verifies that a replaced name inside a string literal is not a use.
///
/// The predicate with the literal is kept, and the predicate that uses `LEVEL` is removed.
#[test]
fn keeps_where_predicate_with_replaced_name_only_in_literal() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize, T>()
        where
            [(); LEVEL]: Sized,
            T: Label<"LEVEL">,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < T > (level : usize) where T : Label < \"LEVEL\" >"
    );
}

/// Verifies that a type path with the name of a replaced const is kept.
///
/// A const replacement only matches paths in expression position.
#[test]
fn keeps_type_position_path_with_same_name_as_replaced_const() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize>()
        where
            LEVEL: Sized,
            [(); LEVEL]: Sized,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy (level : usize) where LEVEL : Sized"
    );
}

/// Verifies that a use inside a block const expression removes its predicate.
///
/// The array length expression `{ LEVEL + 1 }` mentions the replaced const.
#[test]
fn removes_where_predicate_with_replaced_param_inside_block_const_expr() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize, T>()
        where
            [(); { LEVEL + 1 }]: Sized,
            T: Copy,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < T > (level : usize) where T : Copy"
    );
}

/// Verifies that a use inside a conditional const expression removes its predicate.
///
/// The condition of the `if` expression mentions the replaced const.
#[test]
fn removes_where_predicate_with_replaced_param_inside_if_const_expr() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize, T>()
        where
            [(); { if LEVEL > 0 { 1 } else { 0 } }]: Sized,
            T: Copy,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < T > (level : usize) where T : Copy"
    );
}

/// Verifies that an associated const with the name of a replaced const is kept.
///
/// `T::LEVEL` refers to an item of `T`, not to the const parameter.
#[test]
fn keeps_associated_const_path_when_replacing_const_with_same_name() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(LEVEL => level: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<const LEVEL: usize, T>()
        where
            [(); LEVEL]: Sized,
            [(); T::LEVEL]: Sized,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < T > (level : usize) where [() ; T :: LEVEL] : Sized"
    );
}

/// Verifies that a module path ending in the name of a replaced type is kept.
///
/// Only paths that start with the type parameter refer to it.
#[test]
fn keeps_multisegment_path_when_replacing_type_with_same_segment_name() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<T, U>()
        where
            T: Clone,
            module::T: Bound,
            U: Into<T>,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < U > (replacement : Dyn) where module :: T : Bound"
    );
}

/// Verifies that an associated type of a replaced type removes its predicate.
///
/// The path `T::Assoc` starts with the replaced type parameter.
#[test]
fn removes_type_rooted_associated_path_when_replacing_type() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        type(T => replacement: Dyn)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<T, U>()
        where
            T::Assoc: Bound,
            U: Copy,
        {
        }
    };

    let sig = signature_for_copy(&item.sig, &args).unwrap();

    assert_eq!(
        sig.to_token_stream().to_string(),
        "fn copy < U > (replacement : Dyn) where U : Copy"
    );
}

/// Verifies that a const replacement cannot name a type parameter.
///
/// The replacement kind must match the parameter kind.
#[test]
fn rejects_wrong_replacement_kind() {
    let args = parse2::<MacroArgs>(quote! {
        copy,
        const(H => handler: usize)
    })
    .unwrap();
    let item: ItemFn = parse_quote! {
        fn original<H>() {}
    };

    let err = signature_for_copy(&item.sig, &args).unwrap_err();

    assert!(
        err.to_string()
            .contains("const(...) must name a const generic parameter")
    );
}

/// Verifies the expansion of a free function.
///
/// The output contains the original and a copy with the doc comment, the extra attribute, the
/// generated parameter, and the rewritten call.
#[test]
fn expands_free_function_and_copies_doc_attr() {
    let output = maybe_non_generic_impl(
        quote! {
            copy,
            type(H => handler: DynHandler),
            attr(allow(dead_code))
        },
        quote! {
            /// docs
            fn original<H: Handler>() {
                H::run();
            }
        },
    )
    .unwrap()
    .to_string();

    assert!(output.contains("fn original < H : Handler > ()"));
    assert!(output.contains("# [doc ="));
    assert!(output.contains("# [allow (dead_code)] fn copy"));
    assert!(output.contains("fn copy (handler : DynHandler)"));
    assert!(output.contains("(handler . run) ()"));
}

/// Verifies that `dont_copy_attr` keeps the original attributes off the copy.
///
/// Attributes from `attr(...)` are still applied.
#[test]
fn dont_copy_attr_drops_original_attrs() {
    let output = maybe_non_generic_impl(
        quote! {
            copy,
            type(H => handler: DynHandler),
            dont_copy_attr,
            attr(allow(dead_code))
        },
        quote! {
            #[inline]
            fn original<H: Handler>() {
                H::run();
            }
        },
    )
    .unwrap()
    .to_string();

    assert!(output.contains("# [inline] fn original"));
    assert!(!output.contains("# [inline] # [allow (dead_code)] fn copy"));
    assert!(output.contains("# [allow (dead_code)] fn copy"));
    assert!(output.contains("fn copy (handler : DynHandler)"));
}

/// Verifies that stacked `maybe_non_generic` attributes are not copied.
///
/// Other attributes of the original are still copied.
#[test]
fn generated_copy_drops_stacked_maybe_non_generic_attr() {
    let output = maybe_non_generic_impl(
        quote! {
            copy,
            type(H => handler: DynHandler)
        },
        quote! {
            #[maybe_non_generic(other, type(H => handler: DynHandler))]
            #[inline]
            fn original<H: Handler>() {
                H::run();
            }
        },
    )
    .unwrap()
    .to_string();

    assert!(output.contains("# [inline] fn copy"));
    assert!(!output.contains(
        "# [maybe_non_generic (other , type (H => handler : DynHandler))] # [inline] fn copy"
    ));
}

/// Rewrites a block with the given macro arguments.
///
/// Returns the rewritten tokens as a string and panics when the rewrite fails.
fn transformed_block(input: syn::Block, args: &str) -> String {
    let args: MacroArgs = parse_str(args).unwrap();
    let mut block = input;
    transform_body(&mut block, &args).unwrap();
    block.to_token_stream().to_string()
}

/// Returns the error from rewriting a block with the given macro arguments.
///
/// Panics when the rewrite succeeds.
fn transform_block_error(input: syn::Block, args: &str) -> String {
    let args: MacroArgs = parse_str(args).unwrap();
    let mut block = input;
    transform_body(&mut block, &args).unwrap_err().to_string()
}

/// Verifies that associated function calls on a replaced type become field calls.
///
/// The calls go through the function-pointer fields of the generated argument.
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

/// Verifies that an associated item of a replaced type becomes a field access.
///
/// The item is read from the generated argument.
#[test]
fn rewrites_type_static_associated_item_expression_to_field_access() {
    let output = transformed_block(
        parse_quote!({
            let page_size = H::PAGE_SIZE;
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(output.contains("let page_size = handler . PAGE_SIZE"));
}

/// Verifies that a replaced type cannot call an associated function with generic arguments.
///
/// A function-pointer field cannot take generic arguments.
#[test]
fn rejects_generic_type_static_function_call() {
    let error = transform_block_error(
        parse_quote!({
            H::foo::<T>();
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(
        error.contains("generic associated type-parameter access/calls are unsupported"),
        "{error}"
    );
}

/// Verifies that uses of a replaced const become the generated argument.
///
/// Index and arithmetic expressions are both rewritten.
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

/// Verifies that a renamed path call passes its replaced type argument.
///
/// The retained const argument stays in the turbofish, and the type argument becomes a clone of
/// the generated argument.
#[test]
fn rewrites_path_call_and_removes_replaced_turbofish_args() {
    let output = transformed_block(
        parse_quote!({
            let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
        }),
        "copy, type(H => handler: DynPagingHandler), fn(Self::alloc_table => Self::alloc_table_dyn)",
    );

    assert!(
        output.contains(
            "Self :: alloc_table_dyn :: < { M :: LEVELS - 1 } > (:: core :: clone :: Clone :: clone (& handler)) ?",
        ),
        "{output}"
    );
}

/// Verifies that bare const and type turbofish arguments become call arguments.
///
/// The turbofish is removed when no argument remains.
#[test]
fn rewrites_bare_const_and_type_turbofish_args() {
    let output = transformed_block(
        parse_quote!({
            table_of_mut::<LEVEL, H>(paddr);
        }),
        "copy, const(LEVEL => level: usize), type(H => handler: DynPagingHandler)",
    );

    assert!(
        output.contains(
            "table_of_mut (paddr , level , :: core :: clone :: Clone :: clone (& handler))"
        )
    );
    assert!(!output.contains(":: <"));
}

/// Verifies a page-table call path with generic arguments on an earlier segment.
///
/// The renamed call keeps the type arguments of `PageTable` and passes the replaced type argument.
#[test]
fn rewrites_expt_like_path_call_and_removes_replaced_turbofish_args() {
    let output = transformed_block(
        parse_quote!({
            PageTable::<M, PTE>::table_of_mut_non_const::<H>(entry.paddr(), level - 1);
        }),
        "copy, type(H => handler: DynPagingHandler), fn(PageTable::table_of_mut_non_const => PageTable::table_of_mut_non_const_dyn)",
    );

    assert!(
        output.contains(
            "PageTable :: < M , PTE > :: table_of_mut_non_const_dyn (entry . paddr () , level - 1 , :: core :: clone :: Clone :: clone (& handler))"
        ),
        "{output}"
    );
}

/// Verifies a page-table body with an `unsafe` block.
///
/// Const and type uses inside the block are rewritten.
#[test]
fn transforms_table_of_mut_like_body_with_unsafe_block() {
    let output = transformed_block(
        parse_quote!({
            let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];
            unsafe {
                let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
                core::slice::from_raw_parts_mut(ptr, entry_count)
            }
        }),
        "copy, const(LEVEL => level: usize), type(H => handler: DynPagingHandler)",
    );

    assert!(output.contains("let entry_count = M :: LEVEL_TABLE_SIZE [level]"));
    assert!(output.contains("(handler . phys_to_virt) (paddr) . as_mut_ptr_of ()"));
    assert!(!output.contains("LEVEL_TABLE_SIZE [LEVEL]"));
    assert!(!output.contains("H :: phys_to_virt"));
}

/// Verifies a page-table loop with path and method renames.
///
/// Both recursive calls are renamed and receive the replaced type argument.
#[test]
fn transforms_clear_pte_like_loop_fragment() {
    let output = transformed_block(
        parse_quote!({
            let table = PageTable::<M, PTE>::table_of_mut_non_const::<H>(entry.paddr(), level - 1);
            for (index, child) in table.iter_mut().enumerate() {
                let child_vaddr = vaddr + index * M::LEVEL_PAGE_SIZE[level - 1];
                self.clear_pte::<H>(child, level - 1, child_vaddr)?;
            }
            Ok(())
        }),
        "copy, type(H => handler: DynPagingHandler), fn(PageTable::table_of_mut_non_const => PageTable::table_of_mut_non_const_dyn), fn(self.clear_pte => self.clear_pte_dyn)",
    );

    assert!(
        output.contains(
            "PageTable :: < M , PTE > :: table_of_mut_non_const_dyn (entry . paddr () , level - 1 , :: core :: clone :: Clone :: clone (& handler))"
        ),
        "{output}"
    );
    assert!(
        output.contains(
            "self . clear_pte_dyn (child , level - 1 , child_vaddr , :: core :: clone :: Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
    assert!(!output.contains("table_of_mut_non_const :: < H >"));
    assert!(!output.contains("clear_pte :: < H >"));
}

/// Verifies that a replaced const cannot appear in a generic argument expression.
///
/// The expression `{ LEVEL - 1 }` cannot become a call argument.
#[test]
fn rejects_replaced_const_inside_generic_argument_expression() {
    let error = transform_block_error(
        parse_quote!({
            foo::<{ LEVEL - 1 }>();
        }),
        "copy, const(LEVEL => level: usize)",
    );

    assert!(
        error.contains("replaced generic parameters inside generic arguments are unsupported"),
        "{error}"
    );
}

/// Verifies that a replaced type cannot remain in a local type annotation.
///
/// The rewrite does not change types in the body.
#[test]
fn rejects_residual_replaced_type_in_local_annotation() {
    let error = transform_block_error(
        parse_quote!({
            let _: Option<H> = value;
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(
        error.contains("residual use of replaced generic parameter"),
        "{error}"
    );
}

/// Verifies that a replaced type cannot remain in a qualified path call.
///
/// Qualified paths such as `<H as Trait>::foo` are not rewritten.
#[test]
fn rejects_residual_replaced_type_in_qualified_path_call() {
    let error = transform_block_error(
        parse_quote!({
            <H as Trait>::foo();
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(
        error.contains("residual use of replaced generic parameter"),
        "{error}"
    );
}

/// Verifies that a replaced type cannot appear in an earlier path segment.
///
/// Only generic arguments of the last segment become call arguments.
#[test]
fn rejects_replaced_type_in_earlier_path_segment_generic_argument() {
    let error = transform_block_error(
        parse_quote!({
            Wrapper::<H>::make();
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(
        error.contains("replaced generic parameters inside generic arguments are unsupported"),
        "{error}"
    );
}

/// Verifies that a mapped method call is renamed and passes its replaced argument.
///
/// The method turbofish argument becomes a trailing call argument.
#[test]
fn rewrites_method_call_only_with_explicit_mapping() {
    let output = transformed_block(
        parse_quote!({
            self.clear_pte::<H>(child, level - 1, child_vaddr)?;
        }),
        "copy, type(H => handler: DynPagingHandler), fn(self.clear_pte => self.clear_pte_dyn)",
    );

    assert!(
        output.contains(
            "self . clear_pte_dyn (child , level - 1 , child_vaddr , :: core :: clone :: Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
}

/// Verifies that type arguments are passed with a fully qualified `Clone::clone` call.
///
/// The explicit trait call clones the generated argument itself, even when it is a reference, and
/// the absolute path keeps working when another item named `Clone` is in scope.
#[test]
fn clones_type_replacement_with_explicit_clone_trait_call() {
    let output = transformed_block(
        parse_quote!({
            Self::alloc_table::<H>();
        }),
        "copy, type(H => handler: &DynPagingHandler), fn(Self::alloc_table => Self::alloc_table_dyn)",
    );

    assert!(
        output.contains("Self :: alloc_table_dyn (:: core :: clone :: Clone :: clone (& handler))"),
        "{output}"
    );
    assert!(!output.contains("handler . clone ()"));
}

/// Verifies that nested items keep their own generic parameters and bindings.
///
/// A nested function cannot use the generic parameters of the outer function, so its body is not
/// rewritten and its parameters do not conflict with generated parameter names.
#[test]
fn skips_nested_items() {
    let output = transformed_block(
        parse_quote!({
            fn inner<const LEVEL: usize>(level: usize) -> usize {
                LEVEL + level
            }
            inner::<3>(LEVEL)
        }),
        "copy, const(LEVEL => level: usize)",
    );

    assert!(output.contains("LEVEL + level"), "{output}");
    assert!(output.contains("inner :: < 3 > (level)"), "{output}");
}

/// Verifies a mapped method call with several existing arguments.
///
/// The replaced type argument is appended after the existing arguments, and the turbofish is
/// removed.
#[test]
fn rewrites_get_page_entry_mut_method_call_with_retained_type_generic() {
    let output = transformed_block(
        parse_quote!({
            let (entry, index) = self.get_page_entry_mut::<H>(start_vaddr, level, true, true)?;
        }),
        "copy, type(H => handler: DynPagingHandler), fn(self.get_page_entry_mut => self.get_page_entry_mut_dyn)",
    );

    assert!(
        output.contains(
            "self . get_page_entry_mut_dyn (start_vaddr , level , true , true , :: core :: clone :: Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
    assert!(!output.contains("get_page_entry_mut :: < H >"), "{output}");
}

/// Verifies that range expressions are analyzed instead of rejected.
///
/// A range without replaced parameters does not count as a remaining use.
#[test]
fn accepts_range_expression_without_replaced_generic_parameters() {
    let output = transformed_block(
        parse_quote!({
            for level in (0..=M::MAX_PAGE_LEVEL).rev() {
                use_level(level);
            }
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(output.contains("0 ..= M :: MAX_PAGE_LEVEL"), "{output}");
}

/// Verifies that a mapped method call passes its replaced const argument.
///
/// The const argument is moved out of the turbofish instead of being rewritten inside it.
#[test]
fn rewrites_const_turbofish_on_mapped_method_call_before_visiting_generics() {
    let output = transformed_block(
        parse_quote!({
            self.foo::<N>(arg);
        }),
        "copy, const(N => n: usize), fn(self.foo => self.foo_dyn)",
    );

    assert!(output.contains("self . foo_dyn (arg , n)"), "{output}");
    assert!(!output.contains("foo_dyn :: < n >"), "{output}");
}

/// Verifies that a braced const turbofish argument on a mapped method is rejected.
///
/// A braced expression that mentions the replaced const cannot become a call argument.
#[test]
fn rejects_braced_const_turbofish_on_mapped_method_call_before_visiting_generics() {
    let error = transform_block_error(
        parse_quote!({
            self.foo::<{ N }>(arg);
        }),
        "copy, const(N => n: usize), fn(self.foo => self.foo_dyn)",
    );

    assert!(
        error.contains("replaced generic parameters inside generic arguments are unsupported"),
        "{error}"
    );
}

/// Verifies that a local binding cannot use a generated parameter name.
///
/// The binding would shadow the generated parameter in rewritten expressions.
#[test]
fn rejects_body_local_binding_that_conflicts_with_generated_argument() {
    let error = transform_block_error(
        parse_quote!({
            let level = 1;
            let next = LEVEL - 1;
        }),
        "copy, const(LEVEL => level: usize)",
    );

    assert!(
        error.contains("body binding conflicts with a generated argument name"),
        "{error}"
    );
}

/// Verifies that a closure parameter cannot use a generated parameter name.
///
/// Patterns in closures are checked like other body bindings.
#[test]
fn rejects_closure_binding_that_conflicts_with_generated_argument() {
    let error = transform_block_error(
        parse_quote!({
            let f = |handler| {
                H::run();
                handler
            };
        }),
        "copy, type(H => handler: DynHandler)",
    );

    assert!(
        error.contains("body binding conflicts with a generated argument name"),
        "{error}"
    );
}

/// Verifies that a method rename must keep its receiver.
///
/// Only the method name of a call can be replaced.
#[test]
fn rejects_method_replacement_with_different_target_receiver() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        fn(self.foo => other.foo_dyn)
    })
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("method replacement target receiver must match source receiver"),
        "{err}"
    );
}

/// Verifies that a rename cannot turn a path call into a method call.
///
/// The source and target must have the same form.
#[test]
fn rejects_function_replacement_between_path_and_method_targets() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        fn(Self::foo => self.foo_dyn)
    })
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("function replacement source and target must have the same form"),
        "{err}"
    );
}

/// Verifies that an unmapped method call cannot keep a replaced turbofish argument.
///
/// Method turbofish arguments are only moved for mapped methods.
#[test]
fn rejects_unmapped_method_call_with_residual_replaced_turbofish() {
    let error = transform_block_error(
        parse_quote!({
            self.clear_pte::<H>(child, level - 1, child_vaddr)?;
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(
        error.contains("residual use of replaced generic parameter"),
        "{error}"
    );
}
