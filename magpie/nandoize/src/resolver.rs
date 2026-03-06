use std::str::FromStr;

use proc_macro::Span;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn;

use crate::{
    mapping_context,
    syn_utils::{
        self, get_path_from_function_name, get_path_from_function_name_and_generic_context,
        get_type_from_string,
    },
    workflow_structure, NandoKind,
};

fn generate_resolver_branch_body(
    function_name: &str,
    resolver_mapping_context: &mapping_context::ResolverMappingContext,
    is_own_crate: bool,
    preamble: &Vec<String>,
    epilogue: &Vec<String>,
) -> syn::ExprBlock {
    let nando_path = match resolver_mapping_context.generic_mapping_context {
        Some(ref ctx) => {
            get_path_from_function_name_and_generic_context(function_name, is_own_crate, true, ctx)
        }
        None => get_path_from_function_name(function_name, is_own_crate, true),
    };

    let expected_num_args = resolver_mapping_context.mapping.len();
    let mut argument_resolvers: Vec<syn::Stmt> = vec![];
    let mut nando_call_arguments = vec![];

    let mutable_argument_indices = &resolver_mapping_context.mutable_argument_indices;
    for (idx, resolver_mapping) in resolver_mapping_context.mapping.iter().enumerate() {
        let argument_name = syn::Ident::new(
            &format!("{}", resolver_mapping.argument_name),
            Span::mixed_site().into(),
        );

        nando_call_arguments.push(argument_name.clone());
        argument_resolvers.push(
            syn::parse(match resolver_mapping.kind {
                mapping_context::ResolverMappingKind::Value => {
                    let body: syn::Expr = syn::parse(match resolver_mapping.type_str.as_str() {
                        "" => quote! { v.try_into().unwrap() }.into(),
                        ts @ _ => {
                            if ts.len() == 1 {
                                // NOTE so I am going to be super hacky here and just assume that
                                // all generic type identifiers will be a single character long;
                                // can't wait to see how this breaks.
                                quote! { v.try_into().unwrap() }.into()
                            } else {
                                let ty = syn_utils::get_type_from_string(ts);
                                quote! { TryInto::<#ty>::try_into(v).unwrap() }.into()
                            }
                        }
                    })
                    .expect("failed to parse argument resolver body");
                    quote! {
                        let #argument_name = match args.get(#idx).expect(&error_msg) {
                            ResolvedNandoArgument::Value(v) => #body,
                            r @ _ => {
                                eprintln!("unexpected resolved argument type ({:?}) at idx {} of {}, returning early", r, #idx, #function_name);
                                return Err(());
                            }
                        };
                    }
                    .into()
                }
                ref k @ _ => {
                    let ty = match syn_utils::contains_generics(&resolver_mapping.type_str) {
                        true => match resolver_mapping_context.generic_mapping_context {
                            Some(ref ctx) => if k.is_ref_list() {
                                let vec_type = syn::parse_str(&resolver_mapping.type_str).expect("failed to parse reflist type");
                                let inner_type = syn_utils::get_vec_element_type(&vec_type).expect("failed to extract generic inner RefList element type");
                                let inner_type = syn::Type::Path(syn::TypePath {
                                    qself: None,
                                    path: syn_utils::replace_generics_in_type(&quote! { #inner_type }.to_string(), ctx),
                                });

                                syn::parse(quote! { Vec<#inner_type> }.into()).expect("failed to erase generics from reflist type")
                            } else if k.is_maybe_object() {
                                let option_type = syn::parse_str(&resolver_mapping.type_str).expect("failed to parse opt object type");
                                let inner_type = syn_utils::get_option_inner_type(&option_type).expect("failed to extract generic inner object type");
                                let inner_type = syn::Type::Path(syn::TypePath {
                                    qself: None,
                                    path: syn_utils::replace_generics_in_type(&quote! { #inner_type }.to_string(), ctx),
                                });

                                syn::parse(quote! { #inner_type }.into()).expect("failed to erase generics from reflist type")
                            } else {
                                syn::Type::Path(syn::TypePath {
                                    qself: None,
                                    path: syn_utils::replace_generics_in_type(&resolver_mapping.type_str, ctx),
                                })
                            },
                            None => if k.is_maybe_object() {
                                let option_type = syn::parse_str(&resolver_mapping.type_str).expect("failed to parse opt object type");
                                let inner_type = syn_utils::get_option_inner_type(&option_type).expect("failed to extract inner object type from option");
                                syn::parse(quote! { #inner_type }.into()).expect("failed to erase generics from maybe type")
                            } else {
                                syn_utils::get_resolver_type_from_string(&resolver_mapping.type_str)
                            }
                        },
                        false => syn_utils::get_resolver_type_from_string(&resolver_mapping.type_str),
                    };

                    let is_option_type = syn_utils::is_option_type(&ty);

                    match k {
                        mapping_context::ResolverMappingKind::Object => {
                            match resolver_mapping_context.is_read_only() || !mutable_argument_indices.contains(&idx) {
                                true => quote! { let #argument_name = resolve_read_only_object!(args, #idx, error_msg, #ty, ctx.get_log_entry()); }.into(),
                                false => match is_option_type {
                                    false => quote! { let #argument_name = resolve_object!(args, #idx, error_msg, #ty, ctx.get_log_entry()); }.into(),
                                    true => quote! { let #argument_name = resolve_maybe_object!(args, #idx, error_msg, #ty, ctx.get_log_entry()); }.into(),
                                }
                            }
                        },
                        mapping_context::ResolverMappingKind::MaybeObject => {
                            match resolver_mapping_context.is_read_only() || !mutable_argument_indices.contains(&idx) {
                                true => quote! { let #argument_name = resolve_read_only_maybe_object!(args, #idx, error_msg, #ty, ctx.get_log_entry()); }.into(),
                                false => quote! { let #argument_name = resolve_maybe_object!(args, #idx, error_msg, #ty, ctx.get_log_entry()); }.into(),
                            }
                        }
                        mapping_context::ResolverMappingKind::RefList => {
                            let element_type = syn_utils::get_vec_element_type(&ty).expect("failed to extract RefList element type");
                            match resolver_mapping_context.is_read_only() || !mutable_argument_indices.contains(&idx) {
                                true => quote! { let #argument_name = resolve_read_only_objects!(args, #idx, error_msg, #element_type, ctx.get_log_entry()); }.into(),
                                false => quote! { let #argument_name = resolve_objects!(args, #idx, error_msg, #element_type, ctx.get_log_entry()); }.into(),
                            }
                        }
                        _ => unreachable!(),
                    }
                },
            })
            .expect(&format!("failed to parse argument resolver for argument {}", idx)),
        );
    }

    let nando_call = match resolver_mapping_context.returns_result {
        false => quote! {
            #nando_path(&ctx, &resolved_arg_mappings, ctx.get_log_entry(), #(#nando_call_arguments),*);
        },
        true => match resolver_mapping_context.result_type.contains("PhantomData") {
            // FIXME @hack the use of PhantomData in nanotransaction signatures is an annoying hack, but
            // it is necessary in case of a function that contains generic type variables in its
            // signature but doesn't use them (as is the case with, say, kvs::get()). We don't want
            // to attempt to capture the result of these functions because they mean nothing, so in
            // this case we simply act as if the function does not return a result.
            true => quote! {
                #nando_path(&ctx, &resolved_arg_mappings, ctx.get_log_entry(), #(#nando_call_arguments),*);
            },
            false => match resolver_mapping_context.generic_mapping_context.is_none() {
                true => {
                    let return_type = get_type_from_string(&resolver_mapping_context.result_type);
                    let pattern_type = syn::Pat::Type(syn::PatType {
                        attrs: vec![],
                        pat: Box::new(syn::Pat::Ident(syn::PatIdent {
                            attrs: vec![],
                            by_ref: None,
                            mutability: None,
                            ident: syn::Ident::new("res", Span::call_site().into()),
                            subpat: None,
                        })),
                        colon_token: syn::token::Colon::default(),
                        ty: Box::new(return_type),
                    });

                    quote! {
                        let #pattern_type = #nando_path(&ctx, &resolved_arg_mappings, ctx.get_log_entry(), #(#nando_call_arguments),*);
                        let res = NandoResult::from(res);
                        if !ctx.is_part_of_epic() {
                            // let mut activation_handle = activation_handle.lock().unwrap();
                            let mut activation_handle = {
                                let activation_handle = loop {
                                    match activation_handle.try_borrow_mut() {
                                        Err(_) => {},
                                        Ok(h) => break h,
                                    }
                                };
                                activation_handle
                            };
                            activation_handle.append_result(res);
                        } else {
                            ctx.set_ecb_result(res);
                        }
                    }
                }
                false => quote! {
                    let res = #nando_path(&ctx, &resolved_arg_mappings, ctx.get_log_entry(), #(#nando_call_arguments),*);
                    let res = NandoResult::from(res);
                    if !ctx.is_part_of_epic() {
                        // let mut activation_handle = activation_handle.lock().unwrap();
                        let mut activation_handle = activation_handle.borrow_mut();
                        activation_handle.append_result(res);
                    } else {
                        ctx.set_ecb_result(res);
                    }
                },
            },
        },
    };

    let (preamble, epilogue) = match workflow_structure::get_target_item_from_name_if_polymorphic(
        function_name.to_string(),
    ) {
        Err(_) => (preamble.to_vec(), epilogue.to_vec()),
        Ok(s) => match s {
            None => (preamble.to_vec(), epilogue.to_vec()),
            Some((func, _is_polymorphic)) => {
                let log_entry_ident = syn::Ident::new("log_entry", Span::mixed_site().into());
                match crate::generate_tracking_statements(&func, log_entry_ident) {
                    Ok((p, e)) => (p, e),
                    Err(_e) => (preamble.to_vec(), epilogue.to_vec()),
                }
            }
        },
    };

    let preamble: Vec<TokenStream2> = preamble
        .iter()
        .map(|e| {
            TokenStream2::from_str(e).expect("failed to convert preamble entry to tokenstream")
        })
        .collect();
    let mut epilogue: Vec<TokenStream2> = epilogue
        .iter()
        .map(|e| {
            TokenStream2::from_str(e).expect("failed to convert epilogue entry to tokenstream")
        })
        .collect();

    if !resolver_mapping_context.is_read_only() {
        epilogue.push(quote! {
            execution_definitions::versioning::merge_version_constraints(&args);
        });
    }

    syn::parse(
        quote! {
            {
                let error_msg = format!(
                    "invald number of arguments to {}: expected {} but received {}",
                    #function_name,
                    #expected_num_args,
                    args.len(),
                    );

                #(#argument_resolvers)*

                #(#preamble)*

                let ret_val = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    #nando_call
                })) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(()),
                };

                #(#epilogue)*

                ret_val
            }
        }
        .into(),
    )
    .expect("could not parse resolver branch body")
}

// NOTE Ideally, we would construct the `Arm` instances while processing the macro and keep
// them around as part of the `LibFileOutput` struct. However, `Arm`s contain members that are
// wrapped in `Box` pointer types; consequently, if we try to do _anything_ with an Arm that
// was instantiated in a previous invocation of the `nandoize_lib` macro, we will crash with a
// use-after-free error, so we need to re-instantiate all arms every time we update the
// output file. We could avoid this by doing a similar trick as with the `use` statements (i.e.
// parse whatever arms we have already stored in the implementation during a previous run and
// only process any new arms generated during this run), but the matching logic for parsing the
// body of `resolve_function` seems a bit more involved than that of use items, so this is an
// acceptable tradeoff for the time being.
pub(crate) fn generate_arm(
    function_name: &str,
    resolver_mapping_context: &mapping_context::ResolverMappingContext,
    is_own_crate: bool,
    preamble: &Vec<String>,
    epilogue: &Vec<String>,
) -> syn::Arm {
    let branch_body = generate_resolver_branch_body(
        function_name,
        resolver_mapping_context,
        is_own_crate,
        preamble,
        epilogue,
    );

    let function_name = {
        let split_function_name: Vec<String> =
            function_name.split("::").map(|e| e.to_string()).collect();
        match split_function_name.last() {
            None => function_name.to_string(),
            Some(e) => match e.starts_with('<') && e.ends_with('>') {
                false => function_name.to_string(),
                true => split_function_name[0..split_function_name.len() - 1].join("::"),
            },
        }
    };
    syn::parse(
        quote! {
            #function_name => |mut ctx: TxnContext,
                      activation_handle: SharedHandleState,
                      args: &Vec<ResolvedNandoArgument>| {
                ctx.set_namespace(crate::NAMESPACE);
                let resolved_arg_mappings = args.iter().fold(Vec::with_capacity(args.len()), |mut acc, ra| {
                    acc.extend(ra.get_inner_object_arguments().iter().map(|oa| match oa {
                        ObjectArgument::RWObject(o) => o.into_mapping(),
                        ObjectArgument::ROObject(o) => o.into_mapping(),
                        ObjectArgument::UnresolvedObject(_) => unreachable!(),
                    }));

                    acc
                });

                #branch_body
            },
        }
        .into(),
    )
    .expect("failed to parse arm")
}

pub(crate) fn generate_metadata_arm(
    function_name: &str,
    resolver_mapping_context: &mapping_context::ResolverMappingContext,
) -> syn::Arm {
    let ident = syn::Ident::new(
        &resolver_mapping_context.function_kind.to_string(),
        Span::mixed_site().into(),
    );

    let spawns_nandos_lit = syn::LitBool::new(
        resolver_mapping_context.spawns_nandos,
        Span::mixed_site().into(),
    );
    let function_name = {
        let split_function_name: Vec<String> =
            function_name.split("::").map(|e| e.to_string()).collect();
        match split_function_name.last() {
            None => function_name.to_string(),
            Some(e) => match e.starts_with('<') && e.ends_with('>') {
                false => function_name.to_string(),
                true => split_function_name[0..split_function_name.len() - 1].join("::"),
            },
        }
    };

    let mutable_argument_indices = match resolver_mapping_context.function_kind {
        NandoKind::ReadOnly => quote! { None },
        _ => {
            let indices = &resolver_mapping_context.mutable_argument_indices;
            quote! { Some(&[#(#indices),*]) }
        }
    };

    let cache_invalidations_on_completion = match resolver_mapping_context.function_kind {
        NandoKind::ReadOnly => quote! { None },
        _ => match resolver_mapping_context
            .cache_invalidations_on_completion
            .is_empty()
        {
            false => {
                let indices = &resolver_mapping_context.cache_invalidations_on_completion;
                quote! { Some(&[#(#indices),*]) }
            }
            true => quote! { None },
        },
    };

    syn::parse(
        quote! {
            #function_name => NandoMetadata {
                kind: NandoKind::#ident,
                spawns_nandos: #spawns_nandos_lit,
                mutable_argument_indices: #mutable_argument_indices,
                invalidate_on_completion: #cache_invalidations_on_completion,
            }.clone(),
        }
        .into(),
    )
    .unwrap()
}
