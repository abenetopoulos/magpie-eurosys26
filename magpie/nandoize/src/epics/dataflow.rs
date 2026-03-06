//! Rudimentary dataflow analysis over parse trees.
use syn::spanned::Spanned;

use crate::epics::structure;

/// Attempts to detect if the value of an assignment is resolvable without runtime interposition.
///
/// By "unresolvable" here we mean a value that cannot be directly resolved to a concrete value
/// by the user within the context of a nanotransaction -- this includes resolving invariant
/// pointers into concrete references (requires runtime interposition), or any argument that is the
/// result of a `nando_spawn!()`.
pub(crate) fn local_binding_is_unresolvable(
    binding: &syn::Local,
) -> Result<(bool, Option<String>), syn::Error> {
    let binding_ident = extract_ident_from_pattern(&binding.pat)?;
    let mut type_known = false;
    let binding_ident = match &binding.pat {
        syn::Pat::Type(ref pat_type) => {
            type_known = true;
            match type_is_unresolvable(&pat_type.ty) {
                Ok(false) => binding_ident,
                Ok(true) => return Ok((true, binding_ident)),
                Err(e) => return Err(e),
            }
        }
        syn::Pat::Ident(ref pat_ident) => Some(pat_ident.ident.to_string()),
        syn::Pat::Tuple(_) => return Ok((true, None)),
        syn::Pat::TupleStruct(_) => return Ok((false, None)),
        syn::Pat::Wild(_) => return Ok((false, None)),
        t @ _ => todo!("unhandled pattern type in binding: {:?}", t),
    };

    match binding.init {
        Some(ref local_init) => match structure::expr_contains_spawn(&local_init.expr) {
            true => Ok((true, binding_ident)),
            false => Ok((type_known, binding_ident)),
        },
        None => Ok((false, binding_ident)),
    }
}

fn type_is_unresolvable(ty: &syn::Type) -> Result<bool, syn::Error> {
    match ty {
        syn::Type::Path(ref type_path) => {
            for segment in &type_path.path.segments {
                // FIXME @hack this is so sad
                if segment.ident.to_string() == "IPtr" {
                    return Ok(true);
                }

                // TODO we should also check if the segment has arguments, and if so if any of them
                // correspond to unresolvable args.
            }

            Ok(false)
        }
        syn::Type::Array(ref ta) => type_is_unresolvable(&ta.elem),
        syn::Type::Tuple(ref tt) => {
            // check if any of the types in the tuple is unresolved
            for elem in &tt.elems {
                match type_is_unresolvable(elem) {
                    Ok(false) => continue,
                    Ok(true) => return Ok(true),
                    Err(e) => return Err(e),
                }
            }

            Ok(false)
        }
        syn::Type::Macro(_) => Err(syn::Error::new(
            ty.span(),
            "macros in type positions are currently unsupported",
        )),
        syn::Type::Never(_) => Err(syn::Error::new(
            ty.span(),
            "never type is illegal as assignment type",
        )),
        syn::Type::TraitObject(_) => Err(syn::Error::new(
            ty.span(),
            "trait objects inside nanotransactions are currently unsupported",
        )),
        _ => Ok(true),
    }
}

pub(in crate::epics) fn extract_ident_from_pattern(
    pattern: &syn::Pat,
) -> Result<Option<String>, syn::Error> {
    match pattern {
        syn::Pat::Ident(ref pat_ident) => Ok(Some(pat_ident.ident.to_string())),
        syn::Pat::Type(ref pat_type) => extract_ident_from_pattern(&pat_type.pat),
        syn::Pat::Wild(_) => Ok(None),
        // FIXME for tuples we should be able to map `extract_ident_from_pattern` to each of the
        // patterns and return a list of identifiers.
        syn::Pat::Tuple(_) => Ok(None),
        syn::Pat::TupleStruct(_) => Ok(None),
        _ => Err(syn::Error::new(
            pattern.span(),
            "cannot extract assignment target from pattern",
        )),
    }
}
