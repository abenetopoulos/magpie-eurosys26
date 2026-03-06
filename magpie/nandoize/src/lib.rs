//! A collection of procedural macros used to derive nanotransaction wrappers
//! for (almost) arbitrary user-provided Rust functions.
#![feature(proc_macro_def_site, proc_macro_span)]
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Component, Path};

use epics::{optimizations as workflow_optimizations, structure as workflow_structure};
use implementation_output::FileOutput;
use lib_output::LibFileOutput;
use mapping_context::*;
use nando_support::nando_metadata::NandoKind;
use once_cell::sync::Lazy;
use proc_macro::{Span, TokenStream};
use quote::quote;
use syn::{self, parse_macro_input, spanned::Spanned, Meta};

mod contracts;
mod epics;
mod implementation_output;
mod lib_output;
mod mapping_context;
mod resolver;
mod syn_utils;

#[doc(hidden)]
static mut FILE_OUTPUT: Lazy<FileOutput> = Lazy::new(|| FileOutput::new(None).unwrap());

#[doc(hidden)]
static mut LIB_FILE_OUTPUT: Lazy<LibFileOutput> = Lazy::new(|| LibFileOutput::new(None).unwrap());

#[doc(hidden)]
fn get_mutable_file_struct() -> &'static mut FileOutput {
    unsafe { &mut *FILE_OUTPUT }
}

#[doc(hidden)]
fn get_mutable_lib_file_struct() -> &'static mut LibFileOutput {
    unsafe { &mut *LIB_FILE_OUTPUT }
}

#[doc(hidden)]
fn split_path(path: &Path) -> Vec<&str> {
    let module_name = path.file_stem().unwrap();
    let mut path_components: Vec<&str> = vec![];

    for component in path.parent().unwrap().components() {
        if component == Component::Normal(OsStr::new("src")) {
            continue;
        }

        path_components.push(component.as_os_str().to_str().unwrap());
    }

    // NOTE necessary in case we're nando-izing functions (or expanding structs) declared in
    // `src/main.rs`.
    if module_name != "main" {
        path_components.push(module_name.to_str().unwrap());
    }

    path_components
}

#[doc(hidden)]
fn get_use_directive_from_path_components(path: &Path, final_item: Option<&str>) -> syn::ItemUse {
    let mut path_components = split_path(path);
    if let Some(i) = final_item {
        path_components.push(i);
    }

    syn::ItemUse {
        attrs: vec![],
        vis: syn::Visibility::Inherited,
        semi_token: syn::token::Semi::default(),
        leading_colon: Some(syn::token::PathSep::default()),
        use_token: syn::token::Use::default(),
        tree: syn_utils::tree_from_path_components(&path_components),
    }
}

fn get_function_kind(arg_mapping_context: &ArgumentMappingContext) -> NandoKind {
    let mut encountered_read = false;
    let mut encountered_mut = false;
    for arg_mapping in arg_mapping_context.mapping.iter() {
        match arg_mapping {
            ArgumentMapping::Value(v) => {
                if !v.holds_references {
                    continue;
                }

                if v.is_mutable {
                    encountered_mut = true;
                } else {
                    encountered_read = true;
                }
            }
            ArgumentMapping::Reference(rstr) => {
                let reference_mapping_context =
                    arg_mapping_context.reference_map.get(rstr).unwrap();
                if reference_mapping_context.argument_pair.is_mutable {
                    encountered_mut = true;
                } else {
                    encountered_read = true;
                }
            }
        }
    }

    if encountered_read && encountered_mut {
        NandoKind::ReadWrite
    } else if encountered_mut {
        NandoKind::WriteOnly
    } else {
        NandoKind::ReadOnly
    }
}

/// Parses the arguments of a function to be [`nandoize`]d to figure out the nando arguments.
///
/// Parses the arguments of a function [`Signature`] to derive those of interest:
/// - All reference arguments (`&T` and `&mut T`) are to be mapped to the results of [`IPtr`]
///   resolutions.
/// - All other argument types are copied verbatim to the definition of the nando wrapper, and
///   passed directly to the wrapped function.
///
/// [`nandoize`]: attr.nandoize.html
/// [`Signature`]: https://docs.rs/syn/1.0.103/syn/struct.Signature.html
/// [`IPtr`]: ../../object_lib/object/struct.IPtr.html
fn parse_arguments(signature: &syn::Signature) -> Result<ArgumentMappingContext, syn::Error> {
    let mut counter = 0;
    let mut argument_mapping = ArgumentMappingContext::new();

    let pstring_ident = syn::Ident::new("PString", Span::call_site().into());
    let vec_ident = syn::Ident::new("Vec", Span::call_site().into());
    let option_ident = syn::Ident::new("Option", Span::call_site().into());

    for input in &signature.inputs {
        match input {
            syn::FnArg::Receiver(_) => {
                return Err(syn::Error::new(
                    input.span(),
                    "cannot nandoize associated methods",
                ))
            }
            syn::FnArg::Typed(pat_type) => {
                let ty = &pat_type.ty;
                let ident = match syn_utils::get_identifier_from_pattern(&pat_type.pat) {
                    Ok(i) => i,
                    Err(e) => return Err(e),
                };

                match &**ty {
                    syn::Type::Infer(_) => {
                        return Err(syn::Error::new(
                            input.span(),
                            "function parameter types must be explicitly specified",
                        ));
                    }
                    syn::Type::Ptr(_) => {
                        return Err(syn::Error::new(
                            input.span(),
                            "pointer types not supported for function parameters",
                        ));
                    }
                    syn::Type::Reference(rt) => {
                        // NOTE For now, we assume that all references to concrete instances will be
                        // resolved through an invariant ptr.
                        let ty = (*rt.elem).clone();
                        let iptr_ident =
                            syn::Ident::new(&format!("iptr{}", counter), Span::call_site().into());
                        let object_ident =
                            syn::Ident::new(&format!("obj{}", counter), Span::call_site().into());
                        let mut argument_map = ReferenceArgMappingContext::new(
                            ident.clone(),
                            ty,
                            object_ident.clone(),
                            iptr_ident.clone(),
                        );
                        counter += 1;

                        argument_map.argument_pair.is_mutable = rt.mutability.is_some();
                        argument_map.argument_pair.function_argument = quote! { #ident };
                        // argument_map.argument_pair.nando_parameter =
                        //     quote! { #object_ident: &object_lib::Object };
                        let ref_type = pat_type.ty.clone();
                        argument_map.argument_pair.nando_parameter = quote! { #ident: #ref_type };

                        let key = ident.to_string();
                        argument_mapping
                            .mapping
                            .push(ArgumentMapping::Reference(key.clone()));
                        argument_mapping.reference_map.insert(key, argument_map);
                    }
                    syn::Type::Path(type_path) => {
                        let leading_segment = type_path.path.segments.first().unwrap();
                        let mut is_mutable = false;
                        let mut holds_references = false;
                        let mut is_option = false;

                        let ty = match leading_segment.ident == vec_ident {
                            false => match leading_segment.ident == pstring_ident {
                                true => Box::new(syn::Type::Path(syn::TypePath {
                                    qself: None,
                                    path: syn::Path {
                                        leading_colon: None,
                                        segments: syn::punctuated::Punctuated::from_iter(
                                            [
                                                syn::PathSegment {
                                                    ident: syn::Ident::new(
                                                        "object_lib",
                                                        Span::call_site().into(),
                                                    ),
                                                    arguments: syn::PathArguments::None,
                                                },
                                                syn::PathSegment {
                                                    ident: syn::Ident::new(
                                                        "pstring",
                                                        Span::call_site().into(),
                                                    ),
                                                    arguments: syn::PathArguments::None,
                                                },
                                                syn::PathSegment {
                                                    ident: syn::Ident::new(
                                                        "PString",
                                                        Span::call_site().into(),
                                                    ),
                                                    arguments: syn::PathArguments::None,
                                                },
                                            ]
                                            .into_iter(),
                                        ),
                                    },
                                })),
                                false => match leading_segment.ident == option_ident {
                                    true => {
                                        is_option = true;

                                        let syn::PathArguments::AngleBracketed(ref option_type_arg) =
                                            leading_segment.arguments
                                        else {
                                            unreachable!("missing option type argument");
                                        };

                                        let Some(syn::GenericArgument::Type(ref type_arg)) =
                                            option_type_arg.args.first()
                                        else {
                                            unreachable!("invalid type argument in option");
                                        };

                                        if let syn::Type::Reference(ref tr) = type_arg {
                                            holds_references = true;
                                            if tr.mutability.is_some() {
                                                is_mutable = true;
                                            }
                                        }

                                        ty.clone()
                                    }
                                    false => ty.clone(),
                                },
                            },
                            true => {
                                let syn::PathArguments::AngleBracketed(ref vec_type_arg) =
                                    leading_segment.arguments
                                else {
                                    unreachable!("missing vec type argument");
                                };

                                let Some(syn::GenericArgument::Type(ref type_arg)) =
                                    vec_type_arg.args.first()
                                else {
                                    unreachable!("invalid type argument in vec");
                                };

                                if let syn::Type::Reference(ref tr) = type_arg {
                                    holds_references = true;
                                    if tr.mutability.is_some() {
                                        is_mutable = true;
                                    }
                                }

                                ty.clone()
                            }
                        };

                        argument_mapping
                            .mapping
                            .push(ArgumentMapping::Value(ArgumentPair {
                                is_mutable,
                                holds_references,
                                is_option,
                                function_argument: quote! { #ident },
                                nando_parameter: quote! { #ident: #ty },
                                type_str: quote! { #ty }.to_string(),
                            }));
                    }
                    _ => panic!(),
                }
            }
        }
    }

    Ok(argument_mapping)
}

#[doc(hidden)]
fn resolver_argument_mapping_from_fn(
    item_fn: &syn::ItemFn,
    arg_mapping: ArgumentMappingContext,
) -> ResolverMappingContext {
    let mut resolver_mapping_context = ResolverMappingContext::from(arg_mapping);
    if let syn::ItemFn {
        sig:
            syn::Signature {
                output: syn::ReturnType::Type(_, ty),
                ..
            },
        ..
    } = item_fn
    {
        resolver_mapping_context.returns_result = true;
        resolver_mapping_context.result_type = quote! { #ty }.to_string();
    }

    resolver_mapping_context
}

#[doc(hidden)]
fn infer_logging_info(
    item_fn: &syn::ItemFn,
    positional_argument_mapping: &mut ArgumentMappingContext,
) -> Result<(), syn::Error> {
    // get set of expressions that are either read or written to by the nando
    match contracts::extract_effects(&item_fn.block) {
        Ok(effectful_expressions) => {
            let mut iptr_ident_counter = 0;
            for (expr_key, expr) in &effectful_expressions {
                if expr_is_entire_object(expr_key, expr) {
                    // FIXME @hack this is a temporary hack to avoid attempting to store values that
                    // are too large as pre-/post-images in a log entry as would happen in, for
                    // instance, get_difference_slow_set()
                    continue;
                }

                let field_iptr_ident = syn::Ident::new(
                    &format!("{}_{}", expr_key, iptr_ident_counter),
                    Span::call_site().into(),
                );
                iptr_ident_counter += 1;
                let mapping_value =
                    match positional_argument_mapping.reference_map.get_mut(expr_key) {
                        Some(p) => match p.logging_info {
                            Some(_) => p,
                            None => {
                                p.logging_info = Some(LoggingInformation::default());
                                p
                            }
                        },
                        None => continue,
                    };
                let logging_info = mapping_value.logging_info.as_mut().unwrap();

                let ident = &mapping_value.ident;
                let ty = &mapping_value.ty;
                let object_ident = &mapping_value.object_ident;

                let field_iptr_instantiation_expr = quote! {
                    let mut #field_iptr_ident = #object_ident.offset_of(object_lib::unit_ptr_of!(&#expr));
                    #field_iptr_ident.size = nando_support::utils::get_size_of_field(|#ident: #ty| #expr).try_into().unwrap();
                };

                logging_info.exprs.push(expr.clone());
                logging_info.field_iptr_idents.push(field_iptr_ident);
                logging_info
                    .field_iptr_instantiations
                    .push(field_iptr_instantiation_expr);
            }

            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[doc(hidden)]
fn construct_wrapped_function_call(item_fn: &syn::ItemFn) -> syn::ExprPath {
    // This function is responsible for propagating generic arguments from the nando wrapper to
    // the wrapped function call.
    let function_name = &item_fn.sig.ident;
    let sig_generics = &item_fn.sig.generics;

    match sig_generics.lt_token.is_some() {
        false => syn::parse(quote! { #function_name }.into())
            .expect("failed to parse function call as path"),
        true => {
            let idents: Vec<syn::Ident> = sig_generics
                .params
                .iter()
                .map(|g| match g {
                    syn::GenericParam::Type(ref tp) => tp.ident.clone(),
                    syn::GenericParam::Lifetime(_) => {
                        todo!("lifetime annotations for nandos are unsupported")
                    }
                    syn::GenericParam::Const(_) => {
                        todo!("const generic parameters for nandos are unsupported")
                    }
                })
                .collect();

            syn::ExprPath {
                attrs: vec![],
                qself: None,
                path: syn::Path {
                    leading_colon: None,
                    segments: syn::punctuated::Punctuated::from_iter(
                        [syn::PathSegment {
                            ident: function_name.clone(),
                            arguments: syn::PathArguments::AngleBracketed(
                                syn::AngleBracketedGenericArguments {
                                    colon2_token: Some(syn::token::PathSep::default()),
                                    lt_token: syn::token::Lt::default(),
                                    args: syn::punctuated::Punctuated::from_iter(
                                        idents.into_iter().map(|i| {
                                            syn::parse::<syn::GenericArgument>(quote! { #i }.into())
                                                .expect("failed to convert generic argument")
                                        }),
                                    ),
                                    gt_token: syn::token::Gt::default(),
                                },
                            ),
                        }]
                        .into_iter(),
                    ),
                },
            }
        }
    }
}

fn generate_tracking_statements(
    item_fn: &syn::ItemFn,
    log_entry_ident: syn::Ident,
) -> Result<(Vec<String>, Vec<String>), TokenStream> {
    let function_name = &item_fn.sig.ident;
    let mut err = syn::Error::new(
        item_fn.span(),
        format!(
            "Cannot generate tracking statements for function {}: ",
            function_name
        ),
    );

    let mut preamble: Vec<String> = vec![];
    let mut epilogue: Vec<String> = vec![];

    let mut positional_argument_mapping = match parse_arguments(&item_fn.sig) {
        Ok(m) => m,
        Err(e) => {
            err.combine(e);
            return Err(err.to_compile_error().into());
        }
    };

    positional_argument_mapping.function_kind = get_function_kind(&positional_argument_mapping);

    preamble.push(quote! { let log_entry = ctx.get_log_entry(); }.to_string());

    for (arg_idx, argument_mapping) in positional_argument_mapping.mapping.iter().enumerate() {
        match argument_mapping {
            ArgumentMapping::Reference(reference_ident_str) => {
                let reference_mapping = positional_argument_mapping
                    .reference_map
                    .get(reference_ident_str)
                    .expect(&format!(
                        "could not find '{}' in reference map",
                        reference_ident_str
                    ));

                let object_ident = &reference_mapping.object_ident;

                preamble.push(
                    quote! { let #object_ident = args.get(#arg_idx).expect("no arg at idx").get_inner_object_argument().expect("no inner object arg"); }.to_string()
                );

                match &reference_mapping.logging_info {
                    Some(logging_info) => {
                        // we need to create a new pre-image and post-image entry for each
                        // expression that corresponds to a value that the wrapped function
                        // modifies.
                        for (idx, field_iptr) in logging_info.field_iptr_idents.iter().enumerate() {
                            let expr = logging_info.exprs.get(idx).unwrap();
                            preamble.push(
                                logging_info
                                    .field_iptr_instantiations
                                    .get(idx)
                                    .unwrap()
                                    .to_string(),
                            );
                            preamble.push(quote! { #log_entry_ident.borrow_mut().add_new_pre_image(&mut #field_iptr, (#expr).as_bytes()); }.to_string());

                            epilogue.push(quote! { #log_entry_ident.borrow_mut().add_new_post_image_if_changed(&mut #field_iptr, (#expr).as_bytes()); }.to_string());
                        }

                        epilogue.push(
                            quote! { bump_if_changed!(#object_ident, #log_entry_ident); }
                                .to_string(),
                        );
                    }
                    None => {
                        if reference_mapping.argument_pair.is_mutable {
                            epilogue.push(
                                quote! { bump_if_changed!(#object_ident, #log_entry_ident); }
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            _ => continue,
        }
    }

    Ok((preamble, epilogue))
}

#[doc(hidden)]
fn nandoize_core(
    item_fn: syn::ItemFn,
    generate_trait_item: bool,
) -> Result<
    (
        syn::ItemFn,
        Vec<String>,
        Vec<String>,
        ArgumentMappingContext,
        Option<syn::TraitItemFn>,
    ),
    syn::Error,
> {
    let function_name = &item_fn.sig.ident;

    let sig_generics = item_fn.sig.generics.clone();
    let where_clause = sig_generics.where_clause.clone();

    let mut err = syn::Error::new(
        item_fn.span(),
        format!("Cannot nandoize function {}: ", function_name),
    );

    // check if we're being asked to wrap an async function
    if let syn::ItemFn {
        sig: syn::Signature {
            asyncness: Some(_), ..
        },
        ..
    } = item_fn
    {
        err.combine(syn::Error::new(
            item_fn.sig.span(),
            "function is marked `async`",
        ));
        return Err(err);
    }

    let nandoized_function_name = syn::Ident::new(
        &format!("{}_nando", function_name),
        Span::call_site().into(),
    );
    let output_type = &item_fn.sig.output;

    let mut positional_argument_mapping = match parse_arguments(&item_fn.sig) {
        Ok(m) => m,
        Err(e) => {
            err.combine(e);
            return Err(err);
        }
    };
    positional_argument_mapping.function_kind = get_function_kind(&positional_argument_mapping);

    let mut function_arguments = vec![];
    let mut nando_parameters = vec![];

    let mut preamble: Vec<String> = vec![];
    let mut epilogue: Vec<String> = vec![];

    preamble.push(quote! { let log_entry = ctx.get_log_entry(); }.to_string());

    match infer_logging_info(&item_fn, &mut positional_argument_mapping) {
        Ok(()) => {}
        Err(e) => {
            err.combine(e);
            return Err(err);
        }
    }

    for (arg_idx, argument_mapping) in positional_argument_mapping.mapping.iter().enumerate() {
        match argument_mapping {
            ArgumentMapping::Value(argument_pair) => {
                function_arguments.push(argument_pair.function_argument.clone());
                nando_parameters.push(argument_pair.nando_parameter.clone());
            }
            ArgumentMapping::Reference(reference_ident_str) => {
                let reference_mapping = positional_argument_mapping
                    .reference_map
                    .get(reference_ident_str)
                    .expect(&format!(
                        "could not find '{}' in reference map",
                        reference_ident_str
                    ));

                let object_ident = &reference_mapping.object_ident;
                preamble.push(
                    quote! { let #object_ident = args.get(#arg_idx).expect("no arg at idx").get_inner_object_argument().expect("no inner object arg"); }.to_string()
                );

                match &reference_mapping.logging_info {
                    Some(logging_info) => {
                        // we need to create a new pre-image and post-image entry for each
                        // expression that corresponds to a value that the wrapped function
                        // modifies.
                        for (idx, field_iptr) in logging_info.field_iptr_idents.iter().enumerate() {
                            let expr = logging_info.exprs.get(idx).unwrap();
                            preamble.push(
                                logging_info
                                    .field_iptr_instantiations
                                    .get(idx)
                                    .unwrap()
                                    .to_string(),
                            );
                            preamble.push(quote! { log_entry.borrow_mut().add_new_pre_image(&mut #field_iptr, (#expr).as_bytes()); }.to_string());

                            epilogue.push(quote! { log_entry.borrow_mut().add_new_post_image_if_changed(&mut #field_iptr, (#expr).as_bytes()); }.to_string());
                        }

                        epilogue.push(
                            quote! { bump_if_changed!(#object_ident, log_entry); }.to_string(),
                        );
                    }
                    None => {
                        if reference_mapping.argument_pair.is_mutable {
                            epilogue.push(
                                quote! { bump_if_changed!(#object_ident, log_entry); }.to_string(),
                            );
                        }
                    }
                }

                function_arguments.push(reference_mapping.argument_pair.function_argument.clone());
                nando_parameters.push(reference_mapping.argument_pair.nando_parameter.clone());
            }
        }
    }

    let function_call: syn::ExprPath = construct_wrapped_function_call(&item_fn);

    let func: syn::ItemFn = syn::parse(quote! {
        fn #nandoized_function_name #sig_generics (
            ctx: &execution_definitions::txn_context::TxnContext,
            object_arg_mappings: &Vec<object_lib::tls::ObjectMapping>,
            log_entry: std::sync::Arc<std::cell::RefCell<nando_support::log_entry::TransactionLogEntry>>,
            #(#nando_parameters,)*) #output_type
        #where_clause
        {
            object_lib::tls::init_txn_context();
            object_lib::tls::set_txn_meta(ctx.get_log_entry(), object_arg_mappings, ctx.get_ecb());
            object_lib::tls::set_current_namespace(ctx.get_namespace());
            object_tracker::object_tracker_tls::set_thread_local_object_tracker(ctx.get_object_tracker());
            ownership_tracker::ownership_tracker_tls::set_thread_local_ownership_tracker(ctx.get_ownership_tracker());

            #function_call(#(#function_arguments),*)
        }
    }.into()).expect("failed to parse quoted nando wrapper");

    if !generate_trait_item {
        return Ok((func, preamble, epilogue, positional_argument_mapping, None));
    }

    let trait_item: syn::TraitItemFn = syn::parse(quote! {
        fn #nandoized_function_name #sig_generics (
            ctx: &execution_definitions::txn_context::TxnContext,
            object_arg_mappings: &Vec<object_lib::tls::ObjectMapping>,
            log_entry: std::sync::Arc<std::cell::RefCell<nando_support::log_entry::TransactionLogEntry>>,
            #(#nando_parameters,)*) #output_type
        #where_clause;
    }.into()).unwrap();

    Ok((
        func,
        preamble,
        epilogue,
        positional_argument_mapping,
        Some(trait_item),
    ))
}

fn persistable_derive_core(input: syn::DeriveInput) -> Vec<syn::Item> {
    let struct_name = input.ident.clone();

    let mut items_to_add = vec![];
    let struct_file_pathbuf = Span::call_site().source_file().path();
    let struct_file_path = struct_file_pathbuf.as_path();

    let use_directive =
        get_use_directive_from_path_components(&struct_file_path, Some(&struct_name.to_string()));

    items_to_add.push(syn::Item::Use(use_directive));

    items_to_add
}

/// Derive macro used to generate an empty implementation of [`Persistable`]
/// for the specified struct.
///
/// [`Persistable`]: ../object_lib/persistable/trait.Persistable.html
/// # Examples
///
/// ```
/// #[derive(PersistableDerive)]
/// struct TestStruct {
///     x: u32,
///     y: i32,
/// }
/// ```
#[proc_macro_derive(PersistableDerive)]
pub fn persistable_derive(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as syn::DeriveInput);
    let struct_name = derive_input.ident.clone();

    let mut err = syn::Error::new(
        derive_input.span(),
        format!("Cannot derive macro Persistable for {}: ", struct_name),
    );

    // Check if data definition is annotated with `repr(C)`, and error out if not.
    let mut can_process = false;
    for attribute in &derive_input.attrs {
        // we only care about outer attributes
        if let syn::AttrStyle::Inner(_) = attribute.style {
            continue;
        }

        if !attribute.path().is_ident("repr") {
            continue;
        }

        match attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("C") {
                can_process = true;
            }

            Ok(())
        }) {
            Ok(()) => {}
            Err(e) => {
                err.combine(e);
                return err.to_compile_error().into();
            }
        }
    }

    if !can_process {
        err.combine(syn::Error::new(derive_input.span(), "missing `repr(C)`"));

        return err.to_compile_error().into();
    }

    let items_to_add = persistable_derive_core(derive_input.clone());

    let file_output = get_mutable_file_struct();
    file_output.add_struct_items_and_update(&items_to_add);

    let generics = derive_input.generics;
    let where_clause = generics.clone().where_clause;

    quote! {
        #[automatically_derived]
        impl #generics Persistable for #struct_name #generics #where_clause {}
    }
    .into()
}

/// Derive macro used to generate an empty implementation of [`Persistable`]
/// for the specified struct. The difference with the `PersistableDerive` macro is the target
/// output file.
///
/// [`Persistable`]: ../object_lib/persistable/trait.Persistable.html
/// # Examples
///
/// ```
/// #[derive(PersistableDeriveLib)]
/// struct TestStruct {
///     x: u32,
///     y: i32,
/// }
/// ```
#[proc_macro_derive(PersistableDeriveLib)]
pub fn persistable_derive_lib(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as syn::DeriveInput);

    let struct_name = derive_input.ident.clone();

    let mut err = syn::Error::new(
        derive_input.span(),
        format!("Cannot derive macro Persistable for {}: ", struct_name),
    );

    // Check if data definition is annotated with `repr(C)`, and error out if not.
    let mut can_process = false;
    for attribute in &derive_input.attrs {
        // we only care about outer attributes
        if let syn::AttrStyle::Inner(_) = attribute.style {
            continue;
        }

        if !attribute.path().is_ident("repr") {
            continue;
        }

        match attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("C") {
                can_process = true;
            }

            Ok(())
        }) {
            Ok(()) => {}
            Err(e) => {
                err.combine(e);
                return err.to_compile_error().into();
            }
        }
    }

    if !can_process {
        err.combine(syn::Error::new(derive_input.span(), "missing `repr(C)`"));

        return err.to_compile_error().into();
    }

    let generics = derive_input.generics;
    let where_clause = generics.clone().where_clause;

    quote! {
        #[automatically_derived]
        impl #generics Persistable for #struct_name #generics #where_clause {}
    }
    .into()
}

fn expr_is_entire_object(object_expr: &String, field_expr: &syn::Expr) -> bool {
    if let syn::Expr::Path(syn::ExprPath {
        path: syn::Path { segments, .. },
        ..
    }) = field_expr
    {
        if segments.len() == 1 {
            let syn::PathSegment { ident, .. } = segments.first().unwrap();
            return ident.to_string() == *object_expr;
        }
    }

    return false;
}

/// Attribute macro used to generate a nanotransaction out of the given function.
///
/// This macro populates the definition of the [`NandoManager`] trait with all the functions
/// that have this macro as an attribute. This makes them subsequently invokable through the
/// [`NandoManager`] trait implementation of [`NandoManagerBase`].
///
/// Should be used in conjunction with the [`PersistableDerive`] macro to derive structs that can be
/// persisted in an [`Object`].
///
/// [`PersistableDerive`]: crate::persistable_derive
/// [`NandoManager`]: ../nando_lib/nando_manager/trait.NandoManager.html
/// [`NandoManagerBase`]: ../nando_lib/struct.NandoManagerBase.html
/// [`Object`]: ../object_lib/object/struct.Object.html
/// # Examples
///
/// ```
/// #[derive(PersistableDerive)]
/// struct TestStruct {
///     x: u32,
///     y: i32,
/// }
///
/// #[nandoize]
/// pub fn read_x(ts: &TestStruct) -> u32 {
///     ts.x
/// }
/// ```
#[proc_macro_attribute]
pub fn nandoize(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_fn = syn::parse_macro_input!(item as syn::ItemFn);

    let (func, trait_item) = match nandoize_core(item_fn.clone(), true) {
        Ok((f, _, _, _, Some(t))) => (f, t),
        Err(e) => return e.to_compile_error().into(),
        _ => panic!("this should be unreachable"),
    };

    let function_file_pathbuf = Span::call_site().source_file().path();
    let function_file_path = function_file_pathbuf.as_path();

    let function_name = &item_fn.sig.ident;
    let use_directive = get_use_directive_from_path_components(
        &function_file_path,
        Some(&function_name.to_string()),
    );

    let file_output = get_mutable_file_struct();

    file_output.add_struct_items(&vec![syn::Item::Use(use_directive)]);
    file_output.add_trait_item(&syn::TraitItem::Fn(trait_item.clone()));
    file_output.add_impl_function_and_update(&syn::Item::Fn(func.clone()));

    quote! {
        #item_fn
    }
    .into()
}

/// Attribute macro used to generate a nanotransaction out of the given function, and update the
/// associated library's dispatch function..
///
/// This macro generates the `_nando` variants of annotated functions, similar to `nandoize`.
/// Unlike `nandoize`, the generated nanotransactions are expanded in the target library's
/// top-level namespace (as opposed to being added to `NandoManager`). Invocation of a target
/// function is then only a matter of calling the output library's `resolve_function` with the
/// right nanotransaction name.
///
/// Should be used in conjunction with the [`PersistableDeriveLib`] macro to derive structs that can be
/// persisted in an [`Object`].
///
/// [`PersistableDeriveLib`]: crate::persistable_derive_lib
/// [`NandoManagerBase`]: ../nando_lib/struct.NandoManagerBase.html
/// [`Object`]: ../../object_lib/object/struct.Object.html
/// # Examples
///
/// ```
/// #[derive(PersistableDerive)]
/// struct TestStruct {
///     x: u32,
///     y: i32,
/// }
///
/// #[nandoize_lib]
/// pub fn read_x(ts: &TestStruct) -> u32 {
///     ts.x
/// }
/// ```
#[proc_macro_attribute]
pub fn nandoize_lib(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed_attrs = parse_macro_input!(attr with syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated);
    let object_indices_to_invalidate: Vec<usize> = match parsed_attrs.is_empty() {
        true => vec![],
        false => {
            // FIXME this assumes only one object to invalidate
            let syn::Meta::List(ref metalist) = parsed_attrs[0] else {
                panic!("invalid kind of attribute");
            };
            let parsed_lit: syn::LitInt = metalist.parse_args().expect("invalid kind");
            vec![parsed_lit
                .base10_parse::<usize>()
                .expect("failed to parse arg")]
        }
    };
    let item_fn = syn::parse_macro_input!(item as syn::ItemFn);

    let (item_fn, polymorphic_targets) = match workflow_structure::get_polymorphic_targets(&item_fn)
    {
        Ok((f, mut v)) => {
            v.dedup_by(|a, b| a.target_key == b.target_key);
            (f, v)
        }
        Err(e) => return e.to_compile_error().into(),
    };

    let mut polymorphic_target_contexts = Vec::with_capacity(polymorphic_targets.len());
    let mut target_signatures_by_key = HashMap::with_capacity(polymorphic_targets.len());

    // TODO we still need a dynamic shortcut that will do reference checks and short-circuit if the
    // reference we're traversing refers to one of the objects in our argument list
    for polymorphic_target in &polymorphic_targets {
        let mut positional_argument_mapping = match parse_arguments(&polymorphic_target.item_fn.sig)
        {
            Ok(m) => m,
            Err(e) => {
                return e.to_compile_error().into();
            }
        };

        target_signatures_by_key.insert(
            &polymorphic_target.target_key,
            polymorphic_target.item_fn.sig.clone(),
        );

        positional_argument_mapping.function_kind = get_function_kind(&positional_argument_mapping);
        let mut resolver_arg_mapping = resolver_argument_mapping_from_fn(
            &polymorphic_target.item_fn,
            positional_argument_mapping,
        );
        resolver_arg_mapping.generic_mapping_context =
            Some(polymorphic_target.generic_mapping_context.clone());

        if resolver_arg_mapping.returns_result {
            if let Some(t) = &polymorphic_target.user_assigned_type {
                resolver_arg_mapping.result_type = t.to_string();
            }
        }

        resolver_arg_mapping.spawns_nandos = workflow_structure::function_spawns_nandos(&item_fn);
        polymorphic_target_contexts.push((&polymorphic_target.target_key, resolver_arg_mapping));
    }

    let original_spawns_nandos = workflow_structure::function_spawns_nandos(&item_fn);

    #[cfg(feature = "epic_opts")]
    let item_fn = {
        let positional_argument_mapping = match parse_arguments(&item_fn.sig) {
            Ok(m) => m,
            Err(e) => {
                return e.to_compile_error().into();
            }
        };

        match workflow_optimizations::maybe_shortcircuit_spawns(
            &item_fn,
            &positional_argument_mapping,
            &target_signatures_by_key,
        ) {
            Ok(item_fn) => item_fn,
            Err(e) => {
                return e.to_compile_error().into();
            }
        }
    };

    let (mut func, prologue, epilogue, arg_mapping) = match nandoize_core(item_fn.clone(), false) {
        Ok((f, p, e, am, None)) => (f, p, e, am),
        Err(e) => return e.to_compile_error().into(),
        _ => panic!("this should be unreachable"),
    };

    let crate_name = std::env::var("CARGO_PKG_NAME").unwrap();

    if !item_fn.sig.generics.lt_token.is_some() {
        let mut resolver_arg_mapping = resolver_argument_mapping_from_fn(&item_fn, arg_mapping);
        resolver_arg_mapping.spawns_nandos = original_spawns_nandos;
        resolver_arg_mapping.cache_invalidations_on_completion = object_indices_to_invalidate;

        for (function_name, resolver_arg_mapping) in polymorphic_target_contexts {
            let is_own_crate = match function_name.split_once("::") {
                None => true,
                Some((namespace, _)) => namespace == crate_name,
            };

            let lib_file_output = get_mutable_lib_file_struct();
            lib_file_output.add_resolver_arm(
                &function_name,
                resolver_arg_mapping,
                is_own_crate,
                // these will be filled in by the resolver
                vec![],
                vec![],
            );
        }

        let function_name = &item_fn.sig.ident.to_string();
        let lib_file_output = get_mutable_lib_file_struct();
        lib_file_output.add_resolver_arm_and_update(
            &function_name,
            resolver_arg_mapping,
            true,
            prologue,
            epilogue,
        );
    }

    func.vis = syn::Visibility::Public(syn::token::Pub::default());

    quote! {
        #item_fn

        #func
    }
    .into()
}
