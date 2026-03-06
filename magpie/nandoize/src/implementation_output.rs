//! Generator of a concrete `NandoManager` implementation.
use std::fs::{copy, File};
use std::io::{Read, Seek, Write};
use std::path::{self, PathBuf};

use file_lock::{FileLock, FileOptions};
use proc_macro::TokenStream;
use quote::quote;
use syn;

/// Responsible for maintaining the concrete implementation of `NandoManager` up
/// to date as we process [`nandoize`] macros
///
/// [`nandoize`]: ../attr.nandoize.html
pub struct FileOutput {
    intermediate_file_path: Box<path::PathBuf>,
    final_file_path: Box<path::PathBuf>,
    use_items: Vec<syn::Item>,
    last_flushed_use_item_idx: usize,
    impl_functions: Vec<syn::Item>,
    last_flushed_impl_function_idx: usize,
    trait_items: Vec<syn::TraitItem>,
}
unsafe impl Send for FileOutput {}
unsafe impl Sync for FileOutput {}

impl FileOutput {
    pub fn new(filename: Option<&str>) -> Result<Self, std::io::Error> {
        // TODO consider using $OUT_DIR so that `cargo clean` can clean up intermediate versions.
        let intermediate_file_path =
            path::PathBuf::from(&format!("/tmp/nando_manager-{}.rs", std::process::id()));
        let mut final_file_path = std::env::current_dir()?;
        final_file_path.push("nando-lib/src/nando_manager.rs");

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
            syn::parse(quote!{ use std::sync::Arc; }.into()).unwrap(),
            syn::parse(quote!{ use std::cell::RefCell; }.into()).unwrap(),
            syn::parse(quote!{ use object_lib::{self, Persistable, MaterializedObjectVersion}; }.into()).unwrap(),
            syn::parse(quote!{ use object_lib::tls::ObjectMapping; }.into()).unwrap(),
            syn::parse(quote!{ use nando_support; }.into()).unwrap(),
            syn::parse(quote!{ use logging; }.into()).unwrap(),
            syn::parse(quote!{ use nando_support::{activation_intent::NandoResult, log_entry, resolve_object, bump_if_changed}; }.into()).unwrap(),
        ];

        Ok(Self {
            intermediate_file_path: Box::new(filename),
            final_file_path: Box::new(final_file_path),
            use_items,
            last_flushed_use_item_idx: 0,
            impl_functions: vec![],
            last_flushed_impl_function_idx: 0,
            trait_items: vec![],
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
        let new_impl_functions = &self.impl_functions[self.last_flushed_impl_function_idx..];
        let new_trait_items = &self.trait_items[self.last_flushed_impl_function_idx..];

        let mut trait_section: Option<syn::ItemTrait> = None;
        let mut impl_section: Option<syn::ItemImpl> = None;
        for item in syntax.items {
            if let syn::Item::Use(_) = item {
                use_items.push(item);
                continue;
            }

            if let syn::Item::Impl(impl_item) = item {
                impl_section = Some(impl_item.clone());
                continue;
            }

            if let syn::Item::Trait(item_trait) = item {
                trait_section = Some(item_trait.clone());
                continue;
            }
        }

        let attrs = quote! {
            #[automatically_derived]
            #[allow(unused_mut)]
        };
        let new_impl_section = match impl_section {
            None => quote! {
                #attrs
                impl NandoManager for crate::NandoManagerBase {
                    #(#new_impl_functions)*
                }
            },
            Some(item_impl) => {
                let existing_functions = &item_impl.items;

                quote! {
                    #attrs
                    impl NandoManager for crate::NandoManagerBase {
                        #(#existing_functions)*
                        #(#new_impl_functions)*
                    }
                }
            }
        };

        let new_trait_section = match trait_section {
            None => quote! {
                pub trait NandoManager {
                    #(#new_trait_items)*
                }
            },
            Some(item_impl) => {
                let existing_trait_items = &item_impl.items;

                quote! {
                    pub trait NandoManager {
                        #(#existing_trait_items)*
                        #(#new_trait_items)*
                    }
                }
            }
        };

        let output: TokenStream = quote! {
            #![allow(unused_imports)]
            #(#use_items)*

            #(#new_use_items)*

            #new_trait_section

            #new_impl_section
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
        self.last_flushed_impl_function_idx = self.impl_functions.len();

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

    pub fn add_struct_items(&mut self, items: &[syn::Item]) -> () {
        self.use_items.extend(items.iter().cloned());
    }

    pub fn add_struct_items_and_update(&mut self, items: &[syn::Item]) -> () {
        self.add_struct_items(items);
        self.update_and_copy();
    }

    pub fn add_impl_function(&mut self, function: &syn::Item) -> () {
        self.impl_functions.push(function.clone());
    }

    pub fn add_impl_function_and_update(&mut self, function: &syn::Item) -> () {
        self.add_impl_function(function);
        self.update_and_copy();
    }

    pub fn add_trait_item(&mut self, trait_item: &syn::TraitItem) -> () {
        self.trait_items.push(trait_item.clone());
    }
}
