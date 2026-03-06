//! Module-internal structures used while generating nanotransaction variants of user functions.
use std::collections::HashMap;

use nando_support::nando_metadata::NandoKind;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

/// Pair of nanotransaction parameter and corresponding wrapped function argument.
#[derive(Clone, Debug)]
pub(crate) struct ArgumentPair {
    /// Mutability of original function parameter.
    pub is_mutable: bool,
    /// Expression to be used as an argument to the wrapped function.
    pub function_argument: TokenStream2,
    /// Expression to be used as the parameter in the nanotransaction signature that will
    /// eventually be resolved into the corresponding wrapped function argument.
    pub nando_parameter: TokenStream2,

    pub holds_references: bool,
    pub is_option: bool,

    pub type_str: String,
}

impl Default for ArgumentPair {
    fn default() -> Self {
        Self {
            is_mutable: false,
            function_argument: TokenStream2::default(),
            nando_parameter: TokenStream2::default(),
            holds_references: false,
            is_option: false,
            type_str: TokenStream2::default().to_string(),
        }
    }
}

/// Information for logging all (potentially) modified values of a given object.
#[derive(Clone, Debug)]
pub(crate) struct LoggingInformation {
    /// List of effectful expressions for the current object.
    pub exprs: Vec<syn::Expr>,
    /// List of [`IPtr`] instances generated for logging.
    ///
    /// [`IPtr`]: ../object_lib/object/struct.IPtr.html
    pub field_iptr_idents: Vec<syn::Ident>,
    /// Instantiation blocks for generated [`IPtr`] instances.
    ///
    /// [`IPtr`]: ../object_lib/object/struct.IPtr.html
    pub field_iptr_instantiations: Vec<TokenStream2>,
}

impl Default for LoggingInformation {
    fn default() -> Self {
        Self {
            exprs: vec![],
            field_iptr_idents: vec![],
            field_iptr_instantiations: vec![],
        }
    }
}

/// Context for invariant pointer resolution and data logging.
#[derive(Clone, Debug)]
pub(crate) struct ReferenceArgMappingContext {
    /// The original parameter identifier.
    pub ident: syn::Ident,
    /// The original parameter's declared type.
    pub ty: syn::Type,
    /// Identifier of the object that will be used to resolve the concrete argument.
    pub object_ident: syn::Ident,
    /// Invariant pointer that will be used to instantiate the object identified by
    /// `object_ident`.
    #[allow(dead_code)]
    pub iptr_ident: syn::Ident,
    pub logging_info: Option<LoggingInformation>,
    pub argument_pair: ArgumentPair,
}

impl ReferenceArgMappingContext {
    pub(crate) fn new(
        ident: syn::Ident,
        ty: syn::Type,
        object_ident: syn::Ident,
        iptr_ident: syn::Ident,
    ) -> Self {
        Self {
            ident,
            ty,
            object_ident,
            iptr_ident,
            logging_info: None,
            argument_pair: ArgumentPair::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ArgumentMapping {
    Value(ArgumentPair),
    // The associated value is a key into the owning `ArgumentMappingContext`'s reference map.
    Reference(String),
}

/// Parameter mapping context.
///
/// For each function being nando-ized, this struct maintains the mapping between the arguments of
/// the function that is being wrapped with a transactional context and the parameters of the
/// nanotransaction wrapper. For non-reference types, we effectively map via passthrough using the
/// [`ArgumentMapping::Value`] enum.
///
/// [`ArgumentMapping::Value`]: enum.ArgumentMapping.html
#[derive(Clone, Debug)]
pub(crate) struct ArgumentMappingContext {
    /// Order-preserving argument mapping.
    pub mapping: Vec<ArgumentMapping>,
    pub reference_map: HashMap<String, ReferenceArgMappingContext>,

    pub function_kind: NandoKind,
}

impl ArgumentMappingContext {
    pub(crate) fn new() -> Self {
        Self {
            mapping: vec![],
            reference_map: HashMap::new(),
            function_kind: NandoKind::ReadWrite,
        }
    }

    #[allow(dead_code)]
    pub fn is_read_only(&self) -> bool {
        self.function_kind.is_read_only()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResolverMappingKind {
    Object,
    MaybeObject,
    RefList,
    Value,
}

impl ResolverMappingKind {
    pub(crate) fn is_ref_list(&self) -> bool {
        match self {
            Self::RefList => true,
            _ => false,
        }
    }

    pub(crate) fn is_maybe_object(&self) -> bool {
        match self {
            Self::MaybeObject => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolverMapping {
    pub argument_name: String,
    pub kind: ResolverMappingKind,
    pub type_str: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolverMappingContext {
    /// Order-preserving argument mapping.
    pub mapping: Vec<ResolverMapping>,

    pub returns_result: bool,
    pub function_kind: NandoKind,
    pub result_type: String,

    pub spawns_nandos: bool,

    pub generic_mapping_context: Option<GenericVariableMappingContext>,

    pub mutable_argument_indices: Vec<usize>,
    pub cache_invalidations_on_completion: Vec<usize>,
}

impl ResolverMappingContext {
    pub fn is_read_only(&self) -> bool {
        match self.function_kind {
            NandoKind::ReadOnly => true,
            _ => false,
        }
    }
}

impl From<ArgumentMappingContext> for ResolverMappingContext {
    fn from(value: ArgumentMappingContext) -> Self {
        let mut mapping = vec![];
        let mut mutable_argument_indices = vec![];
        for (idx, argument_mapping) in value.mapping.iter().enumerate() {
            mapping.push(match argument_mapping {
                ArgumentMapping::Value(ap) => {
                    let mut kind = ResolverMappingKind::Value;
                    if ap.holds_references {
                        kind = match ap.is_option {
                            false => ResolverMappingKind::RefList,
                            true => ResolverMappingKind::MaybeObject,
                        };

                        if ap.is_mutable {
                            mutable_argument_indices.push(idx);
                        }
                    }

                    ResolverMapping {
                        argument_name: ap.function_argument.to_string(),
                        kind,
                        type_str: ap.type_str.clone(),
                    }
                }
                ArgumentMapping::Reference(s) => {
                    let ref_mapping = value.reference_map.get(s).unwrap();

                    match ref_mapping.argument_pair.is_mutable {
                        false => {}
                        true => mutable_argument_indices.push(idx),
                    }

                    let ty = ref_mapping.ty.clone();
                    ResolverMapping {
                        argument_name: ref_mapping.ident.to_string(),
                        kind: ResolverMappingKind::Object,
                        type_str: quote! { #ty }.to_string(),
                    }
                }
            });
        }

        Self {
            mapping,
            returns_result: false,
            function_kind: value.function_kind,
            result_type: String::default(),
            generic_mapping_context: None,
            spawns_nandos: false,
            mutable_argument_indices,
            cache_invalidations_on_completion: vec![],
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GenericVariableMappingContext {
    name_mapping: HashMap<String, String>,
    pub positional_mapping: Vec<String>,
}

impl GenericVariableMappingContext {
    pub fn new() -> Self {
        Self {
            name_mapping: HashMap::new(),
            positional_mapping: vec![],
        }
    }

    pub fn get_replacement_string(&self) -> String {
        if self.positional_mapping.is_empty() {
            return String::default();
        }

        let mut res = "<".to_owned();
        res.push_str(&self.positional_mapping.join(","));
        res.push('>');

        res
    }

    pub fn insert_positional_mapping(&mut self, concrete_type: String) {
        self.positional_mapping.push(concrete_type);
    }

    pub fn get_mapping_at_position(&self, idx: usize) -> String {
        self.positional_mapping
            .get(idx)
            .expect(&format!(
                "invalid index {} for positional generic argument (max {})",
                idx,
                self.positional_mapping.len()
            ))
            .clone()
    }

    pub fn insert_named_mapping(&mut self, generic_type_var: String, concrete_type: String) {
        self.name_mapping.insert(generic_type_var, concrete_type);
    }

    pub fn get_named_mapping(&self, generic_type_var: &str) -> Option<String> {
        self.name_mapping.get(generic_type_var).cloned()
    }
}
