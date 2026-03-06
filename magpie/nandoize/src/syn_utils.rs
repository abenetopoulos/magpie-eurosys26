//! A collection of utilities to reduce the pain of interacting with [`syn`] when it comes to types
//! and typepaths (and `use` directives)
//!
//! [`syn`]: https://docs.rs/syn/1.0.103/syn/index.html
use proc_macro::{Span, TokenStream};
use syn;

pub(crate) fn get_identifier_from_pattern(pattern: &syn::Pat) -> Result<syn::Ident, syn::Error> {
    match pattern {
        syn::Pat::Ident(pat_ident) => Ok(pat_ident.ident.clone()),
        _ => Err(syn::Error::new(
            Span::call_site().into(),
            &format!("unsupported pattern: {:#?}", pattern),
        )),
    }
}

pub(crate) fn tree_from_path_components(path_components: &[&str]) -> syn::UseTree {
    if path_components.is_empty() {
        panic!("this should not have happened");
    }

    match path_components[..] {
        [m] => syn::UseTree::Name(syn::UseName {
            ident: syn::Ident::new(&format!("{}", m), Span::call_site().into()),
        }),
        _ => {
            let head = path_components[0].replace("-", "_");
            syn::UseTree::Path(syn::UsePath {
                ident: syn::Ident::new(&format!("{}", head), Span::call_site().into()),
                colon2_token: syn::token::PathSep::default(),
                tree: Box::new(tree_from_path_components(&path_components[1..])),
            })
        }
    }
}

#[allow(dead_code)]
pub(crate) fn get_number_of_function_arguments(function_item: &syn::ItemFn) -> u8 {
    function_item.sig.inputs.len().try_into().expect(&format!(
        "number of function arguments of {} higher than 256",
        function_item.sig.ident
    ))
}

pub(crate) fn get_resolver_type_from_string(type_str: &str) -> syn::Type {
    let type_tokenstr = type_str.parse().unwrap();
    syn::parse(type_tokenstr).expect(&format!(
        "could not parse resolver type from string {}",
        type_str
    ))
}

pub(crate) fn get_type_from_string(type_str: &str) -> syn::Type {
    let split_type_str = type_str.split("::");

    syn::Type::Path(syn::TypePath {
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::from_iter(split_type_str.map(|e| {
                match e.contains("Option") || e.contains("Result") || e.contains("Vec") {
                    false => syn::PathSegment {
                        ident: syn::Ident::new(e, Span::call_site().into()),
                        arguments: syn::PathArguments::None,
                    },
                    true => {
                        let (ident, rest) =
                            e.split_once("<").expect("failed to split on angle bracket");
                        let rest = format!("<{}", rest.trim());
                        let ident = syn::Ident::new(ident.trim(), Span::call_site().into());
                        let generics_tokenstream: TokenStream = rest.parse().unwrap();
                        syn::PathSegment {
                            ident,
                            arguments: syn::PathArguments::AngleBracketed(
                                syn::parse(generics_tokenstream).expect("failed to parse generics"),
                            ),
                        }
                    }
                }
            })),
        },
    })
}

pub(crate) fn get_path_from_function_name(
    function_name: &str,
    is_own_crate: bool,
    output_nando_path: bool,
) -> syn::Path {
    if is_own_crate {
        return syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::from_iter(
                [
                    syn::PathSegment {
                        ident: syn::Ident::new("crate", Span::mixed_site().into()),
                        arguments: syn::PathArguments::None,
                    },
                    syn::PathSegment {
                        ident: match output_nando_path {
                            true => syn::Ident::new(
                                &format!("{}_nando", function_name),
                                Span::mixed_site().into(),
                            ),
                            false => syn::Ident::new(function_name, Span::mixed_site().into()),
                        },
                        arguments: syn::PathArguments::None,
                    },
                ]
                .into_iter(),
            ),
        };
    }

    let nando_path: Vec<String> = function_name.split("::").map(|s| s.to_string()).collect();
    let (type_specifier, path_len) = match nando_path.last() {
        None => (None, nando_path.len()),
        Some(ts) => {
            if ts.starts_with("<") && ts.ends_with(">") {
                (Some(ts.clone()), nando_path.len() - 1)
            } else {
                (None, nando_path.len())
            }
        }
    };

    let mut path_segments: Vec<syn::PathSegment> = nando_path[0..path_len - 1]
        .iter()
        .map(|e| syn::PathSegment {
            ident: syn::Ident::new(e, Span::mixed_site().into()),
            arguments: syn::PathArguments::None,
        })
        .collect();

    let function_identifier_str = nando_path.get(path_len - 1).unwrap();
    path_segments.push(syn::PathSegment {
        ident: match output_nando_path {
            true => syn::Ident::new(
                &format!("{}_nando", function_identifier_str),
                Span::mixed_site().into(),
            ),
            false => syn::Ident::new(function_identifier_str, Span::mixed_site().into()),
        },
        arguments: match type_specifier {
            None => syn::PathArguments::None,
            Some(ts) => {
                let ts = &ts[1..ts.len() - 1];
                syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
                    colon2_token: Some(syn::token::PathSep::default()),
                    gt_token: syn::token::Gt::default(),
                    lt_token: syn::token::Lt::default(),
                    args: syn::punctuated::Punctuated::from_iter(ts.split(',').map(|gs| {
                        let gs_str = gs.to_string();
                        let gs = gs_str.trim();
                        syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
                            qself: None,
                            path: syn::Path {
                                leading_colon: None,
                                segments: syn::punctuated::Punctuated::from_iter(
                                    [syn::PathSegment {
                                        ident: syn::Ident::new(gs, Span::mixed_site().into()),
                                        arguments: syn::PathArguments::None,
                                    }]
                                    .into_iter(),
                                ),
                            },
                        }))
                    })),
                })
            }
        },
    });

    syn::Path {
        leading_colon: None,
        segments: syn::punctuated::Punctuated::from_iter(path_segments.iter().map(|ps| ps.clone())),
    }
}

pub(crate) fn get_path_from_function_name_and_generic_context(
    function_name: &str,
    is_own_crate: bool,
    output_nando_path: bool,
    generic_mapping_context: &crate::GenericVariableMappingContext,
) -> syn::Path {
    let resolved_generic_arguments =
        syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
            colon2_token: Some(syn::token::PathSep::default()),
            gt_token: syn::token::Gt::default(),
            lt_token: syn::token::Lt::default(),
            args: syn::punctuated::Punctuated::from_iter(
                generic_mapping_context.positional_mapping.iter().map(|gs| {
                    let gs_str = gs.to_string();
                    let gs = gs_str.trim();
                    syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
                        qself: None,
                        path: syn::Path {
                            leading_colon: None,
                            segments: syn::punctuated::Punctuated::from_iter(
                                [syn::PathSegment {
                                    ident: syn::Ident::new(gs, Span::mixed_site().into()),
                                    arguments: syn::PathArguments::None,
                                }]
                                .into_iter(),
                            ),
                        },
                    }))
                }),
            ),
        });

    let mut nando_path: Vec<String> = function_name.split("::").map(|s| s.to_string()).collect();
    let (_, path_len) = match nando_path.last() {
        None => (None, nando_path.len()),
        Some(ts) => {
            if ts.starts_with("<") && ts.ends_with(">") {
                (Some(ts.clone()), nando_path.len() - 1)
            } else {
                (None, nando_path.len())
            }
        }
    };

    match is_own_crate {
        false => {}
        true => nando_path[0] = "crate".to_string(),
    }

    let mut path_segments: Vec<syn::PathSegment> = nando_path[0..path_len - 1]
        .iter()
        .map(|e| syn::PathSegment {
            ident: syn::Ident::new(e, Span::mixed_site().into()),
            arguments: syn::PathArguments::None,
        })
        .collect();

    let function_identifier_str = nando_path.get(path_len - 1).unwrap();
    path_segments.push(syn::PathSegment {
        ident: match output_nando_path {
            true => syn::Ident::new(
                &format!("{}_nando", function_identifier_str),
                Span::mixed_site().into(),
            ),
            false => syn::Ident::new(function_identifier_str, Span::mixed_site().into()),
        },
        arguments: resolved_generic_arguments,
    });

    syn::Path {
        leading_colon: None,
        segments: syn::punctuated::Punctuated::from_iter(path_segments.iter().map(|ps| ps.clone())),
    }
}

pub(crate) fn replace_generics_in_type(
    type_name_containing_generics: &str,
    generic_mapping_context: &crate::GenericVariableMappingContext,
) -> syn::Path {
    let (type_name_without_generics, generics) =
        strip_trailing_generics(type_name_containing_generics);

    let resolved_generic_arguments =
        syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
            colon2_token: Some(syn::token::PathSep::default()),
            gt_token: syn::token::Gt::default(),
            lt_token: syn::token::Lt::default(),
            args: syn::punctuated::Punctuated::from_iter(generics.iter().map(|g| {
                let gs = generic_mapping_context
                    .get_named_mapping(&g)
                    .expect(&format!("failed to get generic mapping of {}", g));
                let gs_str = gs.to_string();
                let gs = gs_str.trim();
                syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
                    qself: None,
                    path: syn::Path {
                        leading_colon: None,
                        segments: syn::punctuated::Punctuated::from_iter(
                            [syn::PathSegment {
                                ident: syn::Ident::new(gs, Span::mixed_site().into()),
                                arguments: syn::PathArguments::None,
                            }]
                            .into_iter(),
                        ),
                    },
                }))
            })),
        });

    let path_arguments = match type_name_without_generics.len() == 1 {
        true => resolved_generic_arguments,
        false => {
            let mut inner_type = resolved_generic_arguments;
            for type_name in type_name_without_generics[1..].iter().rev() {
                inner_type =
                    syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
                        // NOTE we can probably change this to None, but it doesn't seem to be
                        // causing any issues so whatever
                        colon2_token: Some(syn::token::PathSep::default()),
                        gt_token: syn::token::Gt::default(),
                        lt_token: syn::token::Lt::default(),
                        args: syn::punctuated::Punctuated::from_iter(
                            [syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
                                qself: None,
                                path: syn::Path {
                                    leading_colon: None,
                                    segments: syn::punctuated::Punctuated::from_iter(
                                        [syn::PathSegment {
                                            ident: syn::Ident::new(
                                                type_name,
                                                Span::mixed_site().into(),
                                            ),
                                            arguments: inner_type,
                                        }]
                                        .into_iter(),
                                    ),
                                },
                            }))]
                            .into_iter(),
                        ),
                    });
            }

            inner_type
        }
    };

    let type_name_without_generics = &type_name_without_generics[0];
    syn::Path {
        leading_colon: None,
        segments: syn::punctuated::Punctuated::from_iter(
            [syn::PathSegment {
                ident: syn::Ident::new(type_name_without_generics, Span::mixed_site().into()),
                arguments: path_arguments,
            }]
            .into_iter(),
        ),
    }
}

pub fn contains_generics(target: &str) -> bool {
    let split_target: Vec<String> = target.split("::").map(|s| s.to_string()).collect();

    match split_target.last() {
        None => target.contains('<') && target.contains('>'),
        Some(e) => e.contains('<') && e.contains('>'),
    }
}

// NOTE the below method assumes that all generic type variables will be at the same nesting level.
pub fn strip_trailing_generics(target: &str) -> (Vec<String>, Vec<String>) {
    let split_target: Vec<String> = target.split("::").map(|s| s.to_string()).collect();

    let target_str = match split_target.last() {
        None => target,
        Some(e) => e,
    };

    match target_str.contains('<') && target_str.contains('>') {
        false => (vec![target.to_string()], vec![]),
        true => {
            // NOTE @correctness
            if split_target.len() > 1 {
                // FIXME this might produce the wrong results because of whitespace etc.
                let generics_list = target_str.split(',').map(|s| s.to_string()).collect();
                return (
                    vec![split_target[0..split_target.len() - 1].join("::")],
                    generics_list,
                );
            }

            let innermost_generics_idx = target_str
                .rfind('<')
                .expect("failed to get index of generics left angle bracket");
            let end_idx = target_str
                .find('>')
                .expect("failed to get index of generics right angle bracket");

            let non_generic_types = target_str[0..innermost_generics_idx]
                .split('<')
                .map(|s| s.trim().to_string())
                .collect();
            let generics_list = target_str[(innermost_generics_idx + 1)..end_idx]
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            (non_generic_types, generics_list)
        }
    }
}

pub(crate) fn get_vec_element_type(collection_type: &syn::Type) -> Result<syn::Type, syn::Error> {
    let vec_ident = syn::Ident::new("Vec", Span::call_site().into());

    match collection_type {
        syn::Type::Path(type_path) => {
            let leading_segment = type_path.path.segments.first().unwrap();
            match leading_segment.ident == vec_ident {
                true => {
                    let syn::PathArguments::AngleBracketed(ref vec_type_arg) =
                        leading_segment.arguments
                    else {
                        unreachable!("missing vec type argument");
                    };

                    let Some(syn::GenericArgument::Type(ref type_arg)) = vec_type_arg.args.first()
                    else {
                        unreachable!("invalid type argument in vec");
                    };

                    let ty = if let syn::Type::Reference(ref tr) = type_arg {
                        *tr.elem.clone()
                    } else {
                        type_arg.clone()
                    };

                    Ok(ty)
                }
                false => todo!("error invalid collection type"),
            }
        }
        _ => todo!("error invalid type"),
    }
}

pub(crate) fn get_option_inner_type(option_type: &syn::Type) -> Result<syn::Type, syn::Error> {
    let option_ident = syn::Ident::new("Option", Span::call_site().into());

    match option_type {
        syn::Type::Path(type_path) => {
            let leading_segment = type_path.path.segments.first().unwrap();
            match leading_segment.ident == option_ident {
                true => {
                    let syn::PathArguments::AngleBracketed(ref vec_type_arg) =
                        leading_segment.arguments
                    else {
                        unreachable!("missing vec type argument");
                    };

                    let Some(syn::GenericArgument::Type(ref type_arg)) = vec_type_arg.args.first()
                    else {
                        unreachable!("invalid type argument in vec");
                    };

                    let ty = if let syn::Type::Reference(ref tr) = type_arg {
                        *tr.elem.clone()
                    } else {
                        type_arg.clone()
                    };

                    Ok(ty)
                }
                false => todo!("error invalid option type"),
            }
        }
        _ => todo!("error invalid type"),
    }
}

pub(crate) fn is_option_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(ref type_path) => match type_path.path.segments.first() {
            Some(segment) => {
                let ident = segment.ident.to_string();
                let ident = ident.trim();
                ident == "Option"
            }
            None => false,
        },
        _ => false,
    }
}
