//! Generator of a concrete `resolver.rs` implementation for an external library.
use std::env;
use std::fs::{copy, File};
use std::io::{Read, Seek, Write};
use std::path::{self, PathBuf};

use file_lock::{FileLock, FileOptions};
use proc_macro::{Span, TokenStream};
use quote::quote;
use syn;

use crate::{resolver, ResolverMappingContext};

/// Responsible for maintaining the concrete implementation of function and metadata resolvers for
/// the given library up to date as we process [`nandoize_lib`] macros
///
/// [`nandoize_lib`]: ../attr.nandoize_lib.html
pub struct LibFileOutput {
    intermediate_file_path: Box<path::PathBuf>,
    final_file_path: Box<path::PathBuf>,

    use_items: Vec<syn::Item>,
    last_flushed_use_item_idx: usize,
    resolver_func_info: Vec<(
        String,
        ResolverMappingContext,
        bool,
        Vec<String>,
        Vec<String>,
    )>,
}
unsafe impl Send for LibFileOutput {}
unsafe impl Sync for LibFileOutput {}

impl LibFileOutput {
    pub fn new(filename: Option<&str>) -> Result<Self, std::io::Error> {
        let out_dir = match env::var("PROFILE") {
            Ok(p) => PathBuf::from("target").join(p),
            _ => PathBuf::from("target"),
        };
        let intermediate_file_path =
            path::PathBuf::from(out_dir.join(format!("resolver-gen-{}.rs", std::process::id())));
        let mut final_file_path = std::env::current_dir()?;
        final_file_path.push("src/resolver.rs");

        let filename = match filename {
            Some(f) => path::PathBuf::from(f),
            None => intermediate_file_path,
        };

        // FIXME @lazy just issue this open for file creation/truncation
        let _file = File::options()
            .truncate(true)
            .append(false)
            .create(true)
            .write(true)
            .read(true)
            .open(&filename)?;

        let use_items: Vec<syn::Item> = vec![
            syn::parse(quote!{ use std::marker::PhantomData; }.into()).unwrap(),
            syn::parse(quote!{ use std::sync::{Arc, Mutex}; }.into()).unwrap(),
            syn::parse(quote!{ use std::cell::RefCell; }.into()).unwrap(),
            syn::parse(quote!{ use execution_definitions::activation::{
                ActivationFunction, ResolvedNandoArgument, ObjectArgument,
            }; }.into()).unwrap(),
            syn::parse(quote!{ use execution_definitions::txn_context::TxnContext; }.into()).unwrap(),
            syn::parse(quote!{ use execution_definitions::nando_handle::{HandleCompletionState, SharedHandleState}; }.into()).unwrap(),
            syn::parse(quote!{ use nando_support::{
                activation_intent::{NandoResult, ScalarValue},
                log_entry,
                resolve_object,
                resolve_maybe_object,
                resolve_read_only_object,
                resolve_read_only_maybe_object,
                resolve_objects,
                resolve_read_only_objects,
                bump_if_changed,
            }; }.into()).unwrap(),
            syn::parse(quote!{ use nando_support::nando_metadata::{NandoKind, NandoMetadata}; }.into()).unwrap(),
            syn::parse(quote!{ use nando_support::epic_control::ECB; }.into()).unwrap(),
            syn::parse(quote!{ use object_lib::MaterializedObjectVersion; }.into()).unwrap(),
            syn::parse(quote!{ use object_lib::{IPtr, ObjectId, Persistable}; }.into()).unwrap(),
            syn::parse(quote!{ use object_lib::tls::{self as nando_tls, ObjectMapping, IntoObjectMapping}; }.into()).unwrap(),
            syn::parse(quote!{ use object_tracker::{object_tracker_tls, ObjectTracker}; }.into()).unwrap(),
            syn::parse(quote!{ use ownership_tracker::{ownership_tracker_tls, OwnershipTracker}; }.into()).unwrap(),
            // NOTE this is kind of sucky, but it's the best way I can think of to support
            // declarations in user libraries.
            syn::parse(quote!{ use crate::*; }.into()).unwrap(),
        ];
        Ok(Self {
            intermediate_file_path: Box::new(filename),
            final_file_path: Box::new(final_file_path),
            use_items,
            last_flushed_use_item_idx: 0,
            resolver_func_info: vec![],
        })
    }

    fn lock_file(&self, file_path: &PathBuf) -> FileLock {
        let options = FileOptions::new()
            .truncate(false)
            .append(false)
            .create(false)
            .write(true)
            .read(true);

        match FileLock::lock(file_path, true, options) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to acquire file lock: {}", e);
                panic!("failed to acquire file lock")
            }
        }
    }

    // NOTE given that we do not have any convenient way of knowing when there are no more macros
    // of interest to us, we need to do this horrible thing every time we process a new item that
    // should end up in the implementation file.
    pub fn update_and_copy(&mut self) {
        let mut intermediate_file_lock = self.lock_file(&*self.intermediate_file_path);

        let mut src = String::new();
        intermediate_file_lock
            .file
            .read_to_string(&mut src)
            .expect("Unable to read file");

        let syntax = syn::parse_file(&src).expect("Unable to parse file");

        let mut use_items = vec![];
        let new_use_items = &self.use_items[self.last_flushed_use_item_idx..];
        for item in syntax.items {
            if let syn::Item::Use(_) = item {
                use_items.push(item);
                continue;
            }
        }

        let attrs = quote! {
            #![allow(unused, dead_code)]
        };

        let resolver_expr_ident =
            syn::Ident::new(&format!("intent_name"), Span::mixed_site().into());

        let mut deduped_resolver_func_info = self.resolver_func_info.clone();

        // NOTE it might be better to simply avoid adding duplicate elements to this collection
        // instead of doing deduplication prior to resolver generation, but this will do for now.
        deduped_resolver_func_info.dedup_by_key(|f| f.0.to_string());

        let mut arms: Vec<syn::Arm> = deduped_resolver_func_info
            .iter()
            .map(|f| resolver::generate_arm(&f.0, &f.1, f.2, &f.3, &f.4))
            .collect();
        arms.push(syn::Arm {
            attrs: vec![],
            pat: syn::Pat::Wild(syn::PatWild {
                attrs: vec![],
                underscore_token: syn::token::Underscore::default(),
            }),
            guard: None,
            fat_arrow_token: syn::token::FatArrow::default(),
            body: Box::new(syn::Expr::Return(syn::ExprReturn {
                attrs: vec![],
                return_token: syn::token::Return::default(),
                expr: Some(Box::new(syn::Expr::Path(syn::ExprPath {
                    attrs: vec![],
                    qself: None,
                    path: syn::Path {
                        leading_colon: None,
                        segments: syn::punctuated::Punctuated::from_iter(
                            [syn::PathSegment {
                                ident: syn::Ident::new("None", Span::call_site().into()),
                                arguments: syn::PathArguments::None,
                            }]
                            .into_iter(),
                        ),
                    },
                }))),
            })),
            comma: None,
        });

        let resolver_expr = syn::Expr::Match(syn::ExprMatch {
            attrs: vec![],
            match_token: syn::token::Match::default(),
            expr: Box::new(syn::Expr::Path(syn::ExprPath {
                attrs: vec![],
                qself: None,
                path: syn::Path {
                    leading_colon: None,
                    segments: syn::punctuated::Punctuated::from_iter(
                        [syn::PathSegment {
                            ident: resolver_expr_ident.clone(),
                            arguments: syn::PathArguments::None,
                        }]
                        .into_iter(),
                    ),
                },
            })),
            brace_token: syn::token::Brace::default(),
            arms,
        });

        let mut metadata_resolver_arms: Vec<syn::Arm> = deduped_resolver_func_info
            .into_iter()
            .map(|f| resolver::generate_metadata_arm(&f.0, &f.1))
            .collect();
        metadata_resolver_arms.push(
            syn::parse(
                quote! {
                    _ => NandoMetadata {
                        kind: NandoKind::ReadWrite,
                        spawns_nandos: true,
                        mutable_argument_indices: None,
                        invalidate_on_completion: None,
                    },
                }
                .into(),
            )
            .unwrap(),
        );

        let metadata_resolver_expr = syn::Expr::Match(syn::ExprMatch {
            attrs: vec![],
            match_token: syn::token::Match::default(),
            expr: Box::new(syn::Expr::Path(syn::ExprPath {
                attrs: vec![],
                qself: None,
                path: syn::Path {
                    leading_colon: None,
                    segments: syn::punctuated::Punctuated::from_iter(
                        [syn::PathSegment {
                            ident: resolver_expr_ident,
                            arguments: syn::PathArguments::None,
                        }]
                        .into_iter(),
                    ),
                },
            })),
            brace_token: syn::token::Brace::default(),
            arms: metadata_resolver_arms,
        });

        let output: TokenStream = quote! {
            #attrs
            #(#use_items)*
            #(#new_use_items)*

            #[no_mangle]
            pub fn resolve_function(intent_name: &str) -> Option<Arc<ActivationFunction>> {
                let f = #resolver_expr;

                Some(Arc::new(f))
            }

            #[no_mangle]
            pub fn get_nando_metadata(intent_name: &str) -> NandoMetadata {
                #metadata_resolver_expr
            }
        }
        .into();

        intermediate_file_lock
            .file
            .rewind()
            .expect("failed to rewind");
        intermediate_file_lock
            .file
            .write_all(output.to_string().as_bytes())
            .expect("failed to write");

        self.last_flushed_use_item_idx = self.use_items.len();

        match copy(&*self.intermediate_file_path, &*self.final_file_path) {
            Ok(bt) => assert_eq!(bt, intermediate_file_lock.file.metadata().unwrap().len()),
            Err(e) => {
                eprintln!(
                    "Failed to copy impl file from {} to {}: {}",
                    self.intermediate_file_path.to_str().unwrap(),
                    self.final_file_path.to_str().unwrap(),
                    e,
                );
                panic!("Failed to copy impl file");
            }
        }
    }

    pub(crate) fn add_resolver_arm(
        &mut self,
        function_name: &str,
        resolver_mapping: ResolverMappingContext,
        is_own_crate: bool,
        preamble: Vec<String>,
        epilogue: Vec<String>,
    ) -> () {
        self.resolver_func_info.push((
            function_name.to_string(),
            resolver_mapping,
            is_own_crate,
            preamble,
            epilogue,
        ));
    }

    pub(crate) fn add_resolver_arm_and_update(
        &mut self,
        function_name: &str,
        resolver_mapping: ResolverMappingContext,
        is_own_crate: bool,
        preamble: Vec<String>,
        epilogue: Vec<String>,
    ) -> () {
        self.add_resolver_arm(
            function_name,
            resolver_mapping,
            is_own_crate,
            preamble,
            epilogue,
        );
        self.update_and_copy();
    }
}
