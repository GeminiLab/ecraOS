use super::*;
use quote::{ToTokens, quote};
use syn::{parse_str, parse2};

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

#[test]
fn rejects_grouped_const_rules() {
    let err = parse2::<MacroArgs>(quote! {
        copy,
        const(A => a: usize, B => b: usize)
    })
    .unwrap_err();

    assert!(err.to_string().contains("exactly one replacement"));
}

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

fn transformed_block(input: syn::Block, args: &str) -> String {
    let args: MacroArgs = parse_str(args).unwrap();
    let mut block = input;
    transform_body(&mut block, &args).unwrap();
    block.to_token_stream().to_string()
}

fn transform_block_error(input: syn::Block, args: &str) -> String {
    let args: MacroArgs = parse_str(args).unwrap();
    let mut block = input;
    transform_body(&mut block, &args).unwrap_err().to_string()
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
fn rewrites_type_static_associated_item_expression_to_field_access() {
    let output = transformed_block(
        parse_quote!({
            let page_size = H::PAGE_SIZE;
        }),
        "copy, type(H => handler: DynPagingHandler)",
    );

    assert!(output.contains("let page_size = handler . PAGE_SIZE"));
}

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

    assert!(
        output.contains(
            "Self :: alloc_table_dyn :: < { M :: LEVELS - 1 } > (Clone :: clone (& handler)) ?",
        ),
        "{output}"
    );
}

#[test]
fn rewrites_bare_const_and_type_turbofish_args() {
    let output = transformed_block(
        parse_quote!({
            table_of_mut::<LEVEL, H>(paddr);
        }),
        "copy, const(LEVEL => level: usize), type(H => handler: DynPagingHandler)",
    );

    assert!(output.contains("table_of_mut (paddr , level , Clone :: clone (& handler))"));
    assert!(!output.contains(":: <"));
}

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
            "PageTable :: < M , PTE > :: table_of_mut_non_const_dyn (entry . paddr () , level - 1 , Clone :: clone (& handler))"
        ),
        "{output}"
    );
}

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
            "PageTable :: < M , PTE > :: table_of_mut_non_const_dyn (entry . paddr () , level - 1 , Clone :: clone (& handler))"
        ),
        "{output}"
    );
    assert!(
        output.contains(
            "self . clear_pte_dyn (child , level - 1 , child_vaddr , Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
    assert!(!output.contains("table_of_mut_non_const :: < H >"));
    assert!(!output.contains("clear_pte :: < H >"));
}

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
            "self . clear_pte_dyn (child , level - 1 , child_vaddr , Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
}

#[test]
fn clones_type_replacement_with_explicit_clone_trait_call() {
    let output = transformed_block(
        parse_quote!({
            Self::alloc_table::<H>();
        }),
        "copy, type(H => handler: &DynPagingHandler), fn(Self::alloc_table => Self::alloc_table_dyn)",
    );

    assert!(output.contains("Self :: alloc_table_dyn (Clone :: clone (& handler))"));
    assert!(!output.contains("handler . clone ()"));
}

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
            "self . get_page_entry_mut_dyn (start_vaddr , level , true , true , Clone :: clone (& handler)) ?"
        ),
        "{output}"
    );
    assert!(!output.contains("get_page_entry_mut :: < H >"), "{output}");
}

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
