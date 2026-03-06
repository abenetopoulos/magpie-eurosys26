#![allow(dead_code)]
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use proc_macro2::Span;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;

use crate::syn_utils::strip_trailing_generics;
use crate::GenericVariableMappingContext;

#[derive(Debug)]
struct LibraryTarget {
    dependency_target_namespace: String,
    dependency_target_function_name: String,
    dependency_file_path: PathBuf,
}

#[derive(Debug)]
pub(in crate::epics) struct EpicArgs {
    pub target: syn::Expr,
    pub positional_args: Vec<syn::Expr>,
    pub named_args: Vec<(syn::Ident, syn::Expr)>,
}

#[derive(Debug)]
pub(crate) struct PolymorphicTarget {
    pub(crate) target_key: String,
    pub(crate) item_fn: syn::ItemFn,
    pub(crate) user_assigned_type: Option<String>,
    pub(crate) generic_mapping_context: GenericVariableMappingContext,
}

// NOTE the below implementation is pretty much a direct copy from a syn example
impl Parse for EpicArgs {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        let target: syn::Expr;
        let mut positional_args = Vec::new();
        let mut named_args = Vec::new();

        target = input.parse()?;
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            if input.peek(syn::Ident::peek_any) && input.peek2(syn::Token![=]) {
                while !input.is_empty() {
                    let name: syn::Ident = input.call(syn::Ident::parse_any)?;
                    input.parse::<syn::Token![=]>()?;
                    let value: syn::Expr = input.parse()?;
                    named_args.push((name, value));
                    if input.is_empty() {
                        break;
                    }
                    input.parse::<syn::Token![,]>()?;
                }
                break;
            }
            positional_args.push(input.parse()?);
        }

        Ok(EpicArgs {
            target,
            positional_args,
            named_args,
        })
    }
}

pub(crate) fn get_polymorphic_targets(
    item_fn: &syn::ItemFn,
) -> Result<(syn::ItemFn, Vec<PolymorphicTarget>), syn::Error> {
    let mut output_item_fn = item_fn.clone();
    output_item_fn.block.stmts.truncate(0);
    let mut res = vec![];
    for stmt in &item_fn.block.stmts {
        if !stmt_contains_spawn(stmt) {
            output_item_fn.block.stmts.push(stmt.clone());
            continue;
        }

        let targets = match extract_spawn_targets_from_stmt(stmt) {
            Err(e) => return Err(e),
            Ok(t) => t,
        };

        let target_type = extract_user_assigned_type(stmt);
        match target_type {
            None => {
                output_item_fn.block.stmts.push(stmt.clone());
            }
            Some(_) => {
                output_item_fn
                    .block
                    .stmts
                    .push(rewrite_type_to_pending_arg(&stmt));
            }
        }

        for target in &targets {
            let target_generic_replacement = extract_type_specialization_from_target(&target);

            match get_all_reachable_polymorphic_targets_inclusive(
                &target,
                &target_type,
                target_generic_replacement,
                vec![],
            ) {
                Ok(ts) => res.extend(ts),
                Err(e) => return Err(e),
            }
        }
    }

    Ok((output_item_fn, res))
}

pub(crate) fn function_spawns_nandos(item_fn: &syn::ItemFn) -> bool {
    for stmt in &item_fn.block.stmts {
        if stmt_contains_spawn(stmt) {
            return true;
        }
    }

    return false;
}

fn read_lib_src(library_target: &LibraryTarget) -> Result<Vec<String>, String> {
    let mut library_paths = vec![];
    let dependency_src_paths = match File::options()
        .read(true)
        .create(false)
        .open(&library_target.dependency_file_path)
    {
        Ok(f) => {
            let reader = BufReader::new(f);
            let dependency_src_path_suffix =
                format!("{}/src/lib.rs", library_target.dependency_target_namespace);

            for line in reader.lines() {
                let line = line.unwrap();
                // eprintln!("processing line {:?}", line);
                let Some((target, _)) = line.split_once(":") else {
                    continue;
                };

                let target_path = Path::new(&target);
                match target_path.is_absolute() {
                    // we are processing an external library
                    true => {
                        if !target_path.ends_with(&dependency_src_path_suffix) {
                            continue;
                        }
                    }
                    // we are processing our own library
                    false => {}
                }

                library_paths.push(PathBuf::from(target_path));
            }

            match library_paths.is_empty() {
                false => library_paths,
                true => return Ok(vec![]),
            }
        }
        Err(e) => {
            let dependency_file_path = &library_target.dependency_file_path;
            eprintln!(
                "could not find dependency {}: {}",
                dependency_file_path.to_str().unwrap(),
                e
            );
            return Err(format!(
                "cannot parse dependency information for {}",
                dependency_file_path.to_str().unwrap()
            ));
        }
    };

    let mut dependency_srcs = vec![];
    for dependency_src_path in &dependency_src_paths {
        match File::options()
            .read(true)
            .create(false)
            .open(&dependency_src_path)
        {
            Ok(mut f) => {
                let mut dependency_src = String::default();
                f.read_to_string(&mut dependency_src)
                    .expect("failed to read");
                dependency_srcs.push(dependency_src)
            }
            Err(_) => todo!(),
        }
    }

    Ok(dependency_srcs)
}

fn get_target_item_from_expr_if_polymorphic(
    target_expr: &syn::Expr,
) -> Result<Option<(syn::ItemFn, bool)>, syn::Error> {
    let (target_span, target_symbol) = match target_expr {
        syn::Expr::Lit(target) => match &target.lit {
            syn::Lit::Str(st) => (target.span(), st.value()),
            _ => return Err(syn::Error::new(target.span(), "missing spawn/yield target")),
        },
        _ => {
            return Err(syn::Error::new(
                target_expr.span(),
                "invalid type of spawn/yield target",
            ))
        }
    };

    get_target_item_if_polymorphic(target_span, target_symbol)
}

pub(crate) fn get_target_item_from_name_if_polymorphic(
    fn_name: String,
) -> Result<Option<(syn::ItemFn, bool)>, syn::Error> {
    get_target_item_if_polymorphic(Span::mixed_site(), fn_name)
}

fn get_target_item_if_polymorphic(
    target_span: Span,
    target_symbol: String,
) -> Result<Option<(syn::ItemFn, bool)>, syn::Error> {
    let path: Vec<&str> = target_symbol.split("::").collect();
    let (namespace, function_name) = match path.len() >= 2 {
        true => (path.get(0).unwrap(), path.get(1).unwrap()),
        false => {
            // FIXME @hack to allow pagerank to call a built-in, see comment in
            // `nano4r::partition_round()`
            if path.get(0).unwrap() == &"update_caches" {
                return Ok(None);
            }

            return Err(syn::Error::new(
                target_span,
                "cannot parse spawn target path, namespace missing",
            ));
        }
    };

    let profile = match std::env::var("DEP_PROFILE") {
        Ok(p) => p,
        Err(_) => "debug".to_string(), // this might be surprising, but anyway
    };

    let dependency_file_path = PathBuf::from(format!("target/{}/deps/{}.d", profile, namespace));
    let library_target = LibraryTarget {
        dependency_target_namespace: namespace.to_string(),
        dependency_target_function_name: function_name.to_string(),
        dependency_file_path,
    };

    let lib_srcs = match read_lib_src(&library_target) {
        Ok(srcs) => srcs,
        Err(e) => {
            eprintln!("Could not read lib source: {}", e);
            return Err(syn::Error::new(
                target_span,
                format!(
                    "could not parse dependency information for {}",
                    target_symbol
                ),
            ));
        }
    };

    if lib_srcs.is_empty() {
        return Ok(None);
    }

    for lib_src in &lib_srcs {
        let lib_syntax_tree = match syn::parse_file(&lib_src) {
            Ok(s) => s,
            Err(_e) => {
                return Err(syn::Error::new(
                    target_span,
                    format!("could not parse source of dependency {}", target_symbol),
                ));
            }
        };

        for item in lib_syntax_tree.items {
            let item_fn = match item {
                syn::Item::Fn(item_fn) => item_fn,
                _ => continue,
            };

            let fn_sig = &item_fn.sig;
            if fn_sig.ident.to_string() != *function_name {
                continue;
            }

            if fn_sig.generics.lt_token.is_none() {
                return Ok(Some((item_fn, false)));
            }

            return Ok(Some((item_fn, true)));
        }
    }

    Err(syn::Error::new(
        target_span,
        format!(
            "Could not find '{}' in library {}",
            target_symbol, namespace
        ),
    ))
}

fn string_of_target_expression(target: &syn::Expr) -> String {
    match target {
        syn::Expr::Lit(syn::ExprLit { lit, .. }) => match lit {
            syn::Lit::Str(lit_str) => lit_str.value(),
            _ => panic!("unhandled target expression type {:?}", target),
        },
        _ => panic!("unhandled target expression type {:?}", target),
    }
}

const SPAWN_IDENTS: [&'static str; 8] = [
    "nando_spawn",
    "nando_spawn_polymorphic",
    "nando_spawn_sink",
    "nando_yield",
    "nando_yield_polymorphic",
    "nando_yield_sink",
    "nando_yield_sink_polymorphic",
    "fold_polymorphic",
];

fn is_spawn_statement(ident: &str) -> bool {
    SPAWN_IDENTS.contains(&ident)
}

fn extract_spawn_target_from_macro_stmt(mac: &syn::Macro) -> Result<Option<syn::Expr>, syn::Error> {
    for seg in &mac.path.segments {
        if is_spawn_statement(&seg.ident.to_string()) {
            let token: EpicArgs = mac.parse_body().unwrap();
            return Ok(Some(token.target));
        }
    }

    Ok(None)
}

fn extract_spawn_targets_from_expr(expr: &syn::Expr) -> Result<Vec<syn::Expr>, syn::Error> {
    match expr {
        syn::Expr::Block(syn::ExprBlock { block, .. }) => {
            let uncombined_results = block
                .stmts
                .iter()
                .filter(|s| stmt_contains_spawn(s))
                .map(|s| extract_spawn_targets_from_stmt(s));

            let mut combined_results = Vec::with_capacity(block.stmts.len());
            for partial_result in uncombined_results {
                match partial_result {
                    Err(err) => return Err(err),
                    Ok(v) => combined_results.extend(v),
                }
            }

            Ok(combined_results)
        }
        syn::Expr::ForLoop(syn::ExprForLoop { body, .. })
        | syn::Expr::Loop(syn::ExprLoop { body, .. }) => {
            let uncombined_results = body
                .stmts
                .iter()
                .filter(|s| stmt_contains_spawn(s))
                .map(|s| extract_spawn_targets_from_stmt(s));

            let mut combined_results = Vec::with_capacity(body.stmts.len());
            for partial_result in uncombined_results {
                match partial_result {
                    Err(err) => return Err(err),
                    Ok(v) => combined_results.extend(v),
                }
            }

            Ok(combined_results)
        }
        syn::Expr::Macro(syn::ExprMacro { mac, .. }) => {
            match extract_spawn_target_from_macro_stmt(mac) {
                Ok(expr) => {
                    if let Some(e) = expr {
                        Ok(vec![e])
                    } else {
                        Ok(Vec::default())
                    }
                }
                Err(err) => Err(err),
            }
        }
        syn::Expr::If(syn::ExprIf {
            then_branch,
            else_branch,
            ..
        }) => {
            let mut then_branch_res = {
                let then_branch_uncombined_results = then_branch
                    .stmts
                    .iter()
                    .filter(|s| stmt_contains_spawn(s))
                    .map(|s| extract_spawn_targets_from_stmt(s));

                let mut combined_results = Vec::with_capacity(then_branch.stmts.len());
                for partial_result in then_branch_uncombined_results {
                    match partial_result {
                        Err(err) => return Err(err),
                        Ok(v) => combined_results.extend(v),
                    }
                }

                combined_results
            };

            let Some((_, else_branch_expr)) = else_branch else {
                return Ok(then_branch_res);
            };

            if !expr_contains_spawn(else_branch_expr) {
                return Ok(then_branch_res);
            }

            match extract_spawn_targets_from_expr(else_branch_expr) {
                Ok(r) => {
                    then_branch_res.extend(r);
                    Ok(then_branch_res)
                }
                Err(err) => Err(err),
            }
        }
        syn::Expr::Match(syn::ExprMatch { arms, .. }) => {
            let uncombined_results = arms
                .iter()
                .filter(|a| expr_contains_spawn(&a.body))
                .map(|a| extract_spawn_targets_from_expr(&a.body));

            let mut combined_results = Vec::with_capacity(arms.len());
            for partial_result in uncombined_results {
                match partial_result {
                    Err(err) => return Err(err),
                    Ok(v) => combined_results.extend(v),
                }
            }

            Ok(combined_results)
        }
        _ => Ok(Vec::default()),
    }
}

fn extract_spawn_targets_from_stmt(stmt: &syn::Stmt) -> Result<Vec<syn::Expr>, syn::Error> {
    match stmt {
        syn::Stmt::Macro(syn::StmtMacro { mac, .. }) => {
            match extract_spawn_target_from_macro_stmt(mac) {
                Err(err) => Err(err),
                Ok(Some(e)) => Ok(vec![e]),
                Ok(None) => Ok(vec![]),
            }
        }
        syn::Stmt::Expr(expr, _) => extract_spawn_targets_from_expr(expr),
        syn::Stmt::Local(syn::Local { init, .. }) => {
            let Some(init) = init else {
                return Err(syn::Error::new(
                    stmt.span(),
                    "statement does not contain a spawn/yield invocation",
                ));
            };
            extract_spawn_targets_from_expr(&init.expr)
        }
        _ => Ok(Vec::default()),
    }
}

fn get_all_reachable_polymorphic_targets_inclusive(
    target_expr: &syn::Expr,
    target_type: &Option<String>,
    target_generic_replacement: Option<GenericVariableMappingContext>,
    mut visited_targets: Vec<String>,
) -> Result<Vec<PolymorphicTarget>, syn::Error> {
    let mut res = vec![];
    let (target_fn, target_fn_is_polymorphic) =
        match get_target_item_from_expr_if_polymorphic(target_expr) {
            Ok(t) => match t {
                None => return Ok(res),
                Some((f, is_polymorphic)) => (f, is_polymorphic),
            },
            Err(e) => return Err(e),
        };

    if visited_targets.contains(&target_fn.sig.ident.to_string()) {
        return Ok(res);
    }
    visited_targets.push(target_fn.sig.ident.to_string());

    let mut target_generic_replacement = target_generic_replacement.clone();
    if target_fn_is_polymorphic && target_generic_replacement.is_some() {
        let target_generic_replacement = target_generic_replacement.as_mut().unwrap();
        update_named_generics_mapping_from_signature(&target_fn, target_generic_replacement);
    }

    let stmts = &target_fn.block.stmts;
    for stmt in stmts {
        if !stmt_contains_spawn(stmt) {
            continue;
        }

        let targets = match extract_spawn_targets_from_stmt(stmt) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "Encountered error while attempting to get spawn target in {}",
                    target_fn.sig.ident
                );
                return Err(e);
            }
        };

        let inner_target_type = extract_user_assigned_type(stmt);
        let target_type = if target_type.is_none() {
            &inner_target_type
        } else {
            target_type
        };

        for target in &targets {
            let mut inner_target_generic_replacement =
                extract_type_specialization_from_target(&target);
            if target_generic_replacement.is_some() && inner_target_generic_replacement.is_some() {
                let target_generic_replacement = target_generic_replacement.as_ref().unwrap();
                let inner_target_generic_replacement =
                    inner_target_generic_replacement.as_mut().unwrap();
                for (idx, positional_argument) in inner_target_generic_replacement
                    .positional_mapping
                    .clone()
                    .iter()
                    .enumerate()
                {
                    match target_generic_replacement.get_named_mapping(&positional_argument) {
                        None => continue,
                        Some(m) => {
                            inner_target_generic_replacement.positional_mapping[idx] = m.clone();
                            inner_target_generic_replacement
                                .insert_named_mapping(positional_argument.clone(), m);
                        }
                    }
                }
            }
            match get_all_reachable_polymorphic_targets_inclusive(
                &target,
                target_type,
                inner_target_generic_replacement,
                visited_targets.clone(),
            ) {
                // NOTE this might contain duplicates
                Ok(v) => res.extend(v),
                Err(e) => {
                    eprintln!(
                        "Encountered error while attempting to get reachable targets in {}",
                        target_fn.sig.ident
                    );
                    return Err(e);
                }
            }
        }
    }

    if target_fn_is_polymorphic {
        let target_to_push = string_of_target_expression(target_expr);
        let target_generic_replacement = target_generic_replacement.expect(&format!(
            "no generic mapping context for generic target function {}",
            target_to_push
        ));

        let target_to_push = match target_generic_replacement
            .get_replacement_string()
            .is_empty()
        {
            true => target_to_push,
            false => {
                // discard trailing generics, as we can replace them with concrete arguments
                // through the mapping context.
                let (targets_to_push, _) = strip_trailing_generics(&target_to_push);
                // FIXME this might be wrong
                let target_to_push = targets_to_push.get(0).unwrap();

                format!(
                    "{}::{}",
                    target_to_push,
                    target_generic_replacement.get_replacement_string()
                )
            }
        };

        res.push(PolymorphicTarget {
            target_key: target_to_push.clone(),
            item_fn: target_fn,
            user_assigned_type: target_type.clone(),
            generic_mapping_context: target_generic_replacement,
        });
    }

    Ok(res)
}

pub(in crate::epics) fn stmt_contains_spawn(stmt: &syn::Stmt) -> bool {
    match stmt {
        syn::Stmt::Macro(syn::StmtMacro { mac, .. }) => macro_stmt_is_spawn(mac),
        syn::Stmt::Expr(expr, _) => expr_contains_spawn(expr),
        syn::Stmt::Local(syn::Local { init, .. }) => {
            let Some(init) = init else { return false };
            expr_contains_spawn(&init.expr)
        }
        _ => false,
    }
}

pub(in crate::epics) fn macro_stmt_is_spawn(mac: &syn::Macro) -> bool {
    for seg in &mac.path.segments {
        if is_spawn_statement(&seg.ident.to_string()) {
            let _token: EpicArgs = mac.parse_body().unwrap();
            return true;
        }
    }

    false
}

pub(in crate::epics) fn expr_contains_spawn(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Block(syn::ExprBlock { block, .. }) => {
            block.stmts.iter().any(|s| stmt_contains_spawn(s))
        }
        syn::Expr::ForLoop(syn::ExprForLoop { body, .. }) => {
            body.stmts.iter().any(|s| stmt_contains_spawn(s))
        }
        syn::Expr::Loop(syn::ExprLoop { body, .. }) => {
            body.stmts.iter().any(|s| stmt_contains_spawn(s))
        }
        syn::Expr::Macro(syn::ExprMacro { mac, .. }) => macro_stmt_is_spawn(mac),
        syn::Expr::If(syn::ExprIf {
            then_branch,
            else_branch,
            ..
        }) => {
            let then_contains_spawn = then_branch.stmts.iter().any(|s| stmt_contains_spawn(s));

            then_contains_spawn
                || (else_branch.is_some() && expr_contains_spawn(&else_branch.as_ref().unwrap().1))
        }
        syn::Expr::Match(syn::ExprMatch { arms, .. }) => {
            // TODO check if there is a spawn in the match expression or in one of the arm patterns
            // and mark an error if so.

            arms.iter().any(|arm| expr_contains_spawn(&arm.body))
        }
        syn::Expr::MethodCall(_) | syn::Expr::Call(_) | syn::Expr::Lit(_) | syn::Expr::Cast(_) => {
            // TODO we should probably make sure the callee is not a nando
            false
        }
        _ => {
            // println!("unsupported expression encountered {:?}", expr);
            false
        }
    }
}

// TODO this should not just rely on a type being declared in a local assignment, because there are
// cases where the type parameter is either a property of an argument (which is fine for the most
// part, if the argument is also a parameter of the caller, because we cover this with our argument mapping)
// OR it can be just passed explicitly to the function. We should be able to parse that kind, and
// then we can potentially avoid doing dumb shit like the following.
fn extract_user_assigned_type(stmt: &syn::Stmt) -> Option<String> {
    match stmt {
        syn::Stmt::Local(syn::Local { pat, .. }) => match pat {
            syn::Pat::Type(syn::PatType { ty, .. }) => Some(quote! { #ty }.to_string()),
            _ => None,
        },
        _ => None,
    }
}

// fn extract_type_specialization_from_target(target: &syn::Expr) -> Option<String> {
fn extract_type_specialization_from_target(
    target: &syn::Expr,
) -> Option<GenericVariableMappingContext> {
    match target {
        syn::Expr::Lit(syn::ExprLit { lit, .. }) => match lit {
            syn::Lit::Str(target_str) => {
                let split_target: Vec<String> = target_str
                    .value()
                    .split("::")
                    .map(|s| s.to_string())
                    .collect();

                match split_target.last().unwrap().starts_with('<') {
                    false => None,
                    true => {
                        let replacement_string = split_target.last().unwrap();
                        let type_specializations =
                            replacement_string.replace('<', "").replace('>', "");
                        let type_specializations = type_specializations
                            .split(",")
                            .map(|s| s.trim().to_string());

                        let mut context = GenericVariableMappingContext::new();
                        // context.set_replacement_string(replacement_string.to_string());
                        for type_specialization in type_specializations {
                            context.insert_positional_mapping(type_specialization);
                        }

                        Some(context)
                    }
                }
            }
            _ => panic!("cannot handle literal {:?} as target", lit),
        },
        _ => panic!("cannot handle {:?} as target", target),
    }
}

fn rewrite_type_to_pending_arg(stmt: &syn::Stmt) -> syn::Stmt {
    let mut res = stmt.clone();

    match res {
        syn::Stmt::Local(ref mut local) => match local.pat {
            syn::Pat::Type(ref mut pat_type) => {
                pat_type.ty = Box::new(syn::Type::Path(
                    syn::parse(quote! { nando_support::NandoArgument }.into())
                        .expect("failed to parse rewrite type"),
                ))
            }
            _ => (),
        },
        _ => (),
    }

    res
}

fn update_named_generics_mapping_from_signature(
    item_fn: &syn::ItemFn,
    generic_mapping_context: &mut GenericVariableMappingContext,
) {
    let sig_generics = item_fn.sig.generics.clone();
    if sig_generics.lt_token.is_none() {
        return;
    }

    let generic_idents: Vec<syn::Ident> = sig_generics
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

    for (idx, generic_ident) in generic_idents.iter().enumerate() {
        let concrete_argument = generic_mapping_context.get_mapping_at_position(idx);
        generic_mapping_context.insert_named_mapping(generic_ident.to_string(), concrete_argument);
    }
}
