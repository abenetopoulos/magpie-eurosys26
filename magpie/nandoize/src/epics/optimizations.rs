use std::collections::HashMap;

use quote::{quote, ToTokens};
use syn::spanned::Spanned;

use crate::epics::{dataflow, structure};
use crate::mapping_context;
use crate::syn_utils::get_path_from_function_name;
use crate::ArgumentMappingContext;

/// Attempts to shortcircuit `nando_spawn!()` calls if all arguments are available.
///
/// This method will rewrite all `nando_spawn!()` invocations into direct function calls, provided
/// that it can detect with certainty that all dependencies are immediately resolvable (see
/// [`local_binding_is_unresolvable`] for an explanation).
///
/// [`local_binding_is_unresolvable`]: ../dataflow/fn.local_binding_is_unresolvable.html
pub(crate) fn maybe_shortcircuit_spawns(
    item_fn: &syn::ItemFn,
    argument_mapping_context: &ArgumentMappingContext,
    target_signatures_by_key: &HashMap<&String, syn::Signature>,
) -> Result<syn::ItemFn, syn::Error> {
    let mut output_item_fn = item_fn.clone();
    output_item_fn.block.stmts.truncate(0);

    let mut binding_is_unresolvable =
        HashMap::with_capacity(argument_mapping_context.mapping.len());
    for arg_mapping in argument_mapping_context.mapping.iter() {
        match arg_mapping {
            mapping_context::ArgumentMapping::Value(argument_pair) => {
                let argument_ident = match argument_pair.nando_parameter.to_string().split_once(':')
                {
                    None => argument_pair.nando_parameter.to_string(),
                    Some((ident, _)) => ident.trim().to_string(),
                };
                binding_is_unresolvable.insert(argument_ident, false);
            }
            mapping_context::ArgumentMapping::Reference(rstr) => {
                // FIXME do we need to strip the type from here as well?
                let reference_mapping_context =
                    argument_mapping_context.reference_map.get(rstr).unwrap();
                binding_is_unresolvable.insert(reference_mapping_context.ident.to_string(), false);
            }
        }
    }

    for stmt in &item_fn.block.stmts {
        match stmt {
            syn::Stmt::Local(ref binding) => {
                // Only checks if the lhs of the assignment corresponds to a resolvable value.
                let stmt_bindings = get_bindings_from_stmt(stmt)?;
                binding_is_unresolvable.extend(stmt_bindings);

                let Some(ref init_expr) = binding.init else {
                    output_item_fn.block.stmts.push(stmt.clone());
                    continue;
                };

                if !structure::expr_contains_spawn(&init_expr.expr) {
                    output_item_fn.block.stmts.push(stmt.clone());
                    continue;
                }

                let mut output_binding = binding.clone();
                let should_return_value = match dataflow::extract_ident_from_pattern(&binding.pat)?
                {
                    None => false,
                    _ => true,
                };
                let mut did_rewrite = false;
                let rewritten_init_expr = match shortcircuit_eligible_spawns_in_expr(
                    &init_expr.expr,
                    &binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                )? {
                    RewriteResult::NoRewrite => (*init_expr.expr).clone(),
                    RewriteResult::Rewrite(rw) => {
                        did_rewrite = true;
                        rw
                    }
                };
                let rewritten_diverge_expr = match init_expr.diverge {
                    None => None,
                    Some((tok, ref diverge_expr)) => {
                        match shortcircuit_eligible_spawns_in_expr(
                            diverge_expr,
                            &binding_is_unresolvable,
                            target_signatures_by_key,
                            should_return_value,
                        )? {
                            RewriteResult::NoRewrite => init_expr.diverge.clone(),
                            RewriteResult::Rewrite(rw) => {
                                did_rewrite = true;
                                Some((tok, Box::new(rw)))
                            }
                        }
                    }
                };

                // _potentially_ rewrite binding target type
                if !should_return_value && did_rewrite {
                    output_binding.pat = strip_type_from_pattern(&binding.pat)?;
                }

                output_binding.init = Some(syn::LocalInit {
                    eq_token: syn::token::Eq::default(),
                    expr: Box::new(rewritten_init_expr),
                    diverge: rewritten_diverge_expr,
                });

                output_item_fn
                    .block
                    .stmts
                    .push(syn::Stmt::Local(output_binding));
            }
            syn::Stmt::Macro(syn::StmtMacro { ref mac, .. }) => {
                if !structure::macro_stmt_is_spawn(mac) {
                    output_item_fn.block.stmts.push(stmt.clone());
                    continue;
                }

                match shortcircuit_spawn_macro_if_eligible(
                    mac,
                    &binding_is_unresolvable,
                    target_signatures_by_key,
                    false,
                ) {
                    Ok(RewriteResult::Rewrite(rw)) => {
                        output_item_fn.block.stmts.push(syn::Stmt::Expr(
                            syn::Expr::Block(syn::ExprBlock {
                                attrs: vec![],
                                label: None,
                                block: rw,
                            }),
                            Some(syn::token::Semi::default()),
                        ))
                    }
                    Ok(RewriteResult::NoRewrite) => output_item_fn.block.stmts.push(stmt.clone()),
                    Err(e) => return Err(e),
                }
            }
            _ => {
                output_item_fn.block.stmts.push(stmt.clone());
            }
        }
    }

    Ok(output_item_fn)
}

fn get_bindings_from_stmt(stmt: &syn::Stmt) -> Result<HashMap<String, bool>, syn::Error> {
    let mut binding_is_unresolvable = HashMap::new();

    match stmt {
        syn::Stmt::Local(ref binding) => match dataflow::local_binding_is_unresolvable(binding) {
            Ok((true, Some(ident))) => {
                binding_is_unresolvable.insert(ident, true);
            }
            Ok((false, Some(ident))) => {
                binding_is_unresolvable.insert(ident, false);
            }
            Ok((_, None)) => (),
            Err(e) => return Err(e),
        },
        syn::Stmt::Expr(ref expr, ref _semi) => {
            let expr_bindings = get_bindings_from_expr(expr)?;
            binding_is_unresolvable.extend(expr_bindings);
        }
        _ => (),
    }

    Ok(binding_is_unresolvable)
}

fn get_bindings_from_expr(expr: &syn::Expr) -> Result<HashMap<String, bool>, syn::Error> {
    let mut binding_is_unresolvable = HashMap::new();

    match expr {
        syn::Expr::If(ref expr_if) => {
            for stmt in &expr_if.then_branch.stmts {
                binding_is_unresolvable.extend(get_bindings_from_stmt(stmt)?);
            }

            if let Some((_, ref else_expr)) = expr_if.else_branch {
                binding_is_unresolvable.extend(get_bindings_from_expr(else_expr)?);
            }
        }
        syn::Expr::ForLoop(ref expr_for_loop) => {
            for stmt in &expr_for_loop.body.stmts {
                binding_is_unresolvable.extend(get_bindings_from_stmt(stmt)?);
            }
        }
        syn::Expr::Block(ref expr_block) => {
            for stmt in &expr_block.block.stmts {
                binding_is_unresolvable.extend(get_bindings_from_stmt(stmt)?);
            }
        }
        e @ _ => panic!("cannot get bindings of syn::Expr {:?}", e),
    }

    Ok(binding_is_unresolvable)
}

fn shortcircuit_eligible_spawns_in_stmt(
    stmt: &syn::Stmt,
    binding_is_unresolvable: &HashMap<String, bool>,
    target_signatures_by_key: &HashMap<&String, syn::Signature>,
    should_return_value: bool,
) -> Result<RewriteResult<syn::Stmt>, syn::Error> {
    match stmt {
        syn::Stmt::Local(ref local) => {
            let local_init = match &local.init {
                Some(i) => i,
                None => return Ok(RewriteResult::NoRewrite),
            };

            let mut output_local = local.clone();
            output_local.init = match shortcircuit_eligible_spawns_in_expr(
                &local_init.expr,
                binding_is_unresolvable,
                target_signatures_by_key,
                true,
            ) {
                Ok(RewriteResult::Rewrite(rewritten_init_expr)) => {
                    let mut local_init = local_init.clone();
                    *local_init.expr = rewritten_init_expr;
                    Some(local_init)
                }
                Ok(RewriteResult::NoRewrite) => Some(local_init.clone()),
                Err(e) => return Err(e),
            };

            Ok(RewriteResult::Rewrite(syn::Stmt::Local(output_local)))
        }
        syn::Stmt::Expr(ref e, s) => {
            let rewritten_expr = shortcircuit_eligible_spawns_in_expr(
                e,
                binding_is_unresolvable,
                target_signatures_by_key,
                should_return_value,
            )?;

            match rewritten_expr {
                RewriteResult::Rewrite(rewritten_expr) => {
                    Ok(RewriteResult::Rewrite(syn::Stmt::Expr(rewritten_expr, *s)))
                }
                RewriteResult::NoRewrite => Ok(RewriteResult::NoRewrite),
            }
        }
        syn::Stmt::Macro(_) => todo!(),
        syn::Stmt::Item(_) => todo!(),
    }
}

fn shortcircuit_eligible_spawns_in_expr(
    expr: &syn::Expr,
    binding_is_unresolvable: &HashMap<String, bool>,
    target_signatures_by_key: &HashMap<&String, syn::Signature>,
    should_return_value: bool,
) -> Result<RewriteResult<syn::Expr>, syn::Error> {
    match expr {
        syn::Expr::Block(ref expr_block) => {
            let mut expr_block_result = expr_block.clone();
            expr_block_result.block.stmts.truncate(0);

            let mut did_rewrite = false;
            for stmt in &expr_block.block.stmts {
                if !structure::stmt_contains_spawn(stmt) {
                    expr_block_result.block.stmts.push(stmt.clone());
                    continue;
                }

                match shortcircuit_eligible_spawns_in_stmt(
                    stmt,
                    binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                ) {
                    Ok(RewriteResult::NoRewrite) => {
                        expr_block_result.block.stmts.push(stmt.clone())
                    }
                    Ok(RewriteResult::Rewrite(stmt_rewrite)) => {
                        did_rewrite = true;
                        expr_block_result.block.stmts.push(stmt_rewrite);
                    }
                    Err(e) => return Err(e),
                }
            }

            match did_rewrite {
                true => Ok(RewriteResult::Rewrite(syn::Expr::Block(expr_block_result))),
                false => Ok(RewriteResult::NoRewrite),
            }
        }
        syn::Expr::If(ref expr_if) => {
            let mut expr_if_result = expr_if.clone();
            expr_if_result.then_branch.stmts.truncate(0);

            for stmt in &expr_if.then_branch.stmts {
                let output_stmt = match shortcircuit_eligible_spawns_in_stmt(
                    stmt,
                    binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                )? {
                    RewriteResult::NoRewrite => stmt.clone(),
                    RewriteResult::Rewrite(rw) => rw,
                };

                expr_if_result.then_branch.stmts.push(output_stmt);
            }

            if let Some((tok, ref else_expr)) = expr_if.else_branch {
                match shortcircuit_eligible_spawns_in_expr(
                    else_expr,
                    binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                ) {
                    Ok(RewriteResult::Rewrite(e)) => {
                        expr_if_result.else_branch = Some((tok, Box::new(e)));
                    }
                    Ok(RewriteResult::NoRewrite) => {}
                    Err(e) => return Err(e),
                }
            }

            Ok(RewriteResult::Rewrite(syn::Expr::If(expr_if_result)))
        }
        syn::Expr::ForLoop(ref expr_for_loop) => {
            let mut expr_for_loop_result = expr_for_loop.clone();
            expr_for_loop_result.body.stmts.truncate(0);

            for stmt in &expr_for_loop.body.stmts {
                let output_stmt = match shortcircuit_eligible_spawns_in_stmt(
                    stmt,
                    binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                )? {
                    RewriteResult::NoRewrite => stmt.clone(),
                    RewriteResult::Rewrite(rw) => rw,
                };

                expr_for_loop_result.body.stmts.push(output_stmt);
            }

            Ok(RewriteResult::Rewrite(syn::Expr::ForLoop(
                expr_for_loop_result,
            )))
        }
        syn::Expr::Loop(ref expr_loop) => {
            let mut expr_loop_result = expr_loop.clone();
            expr_loop_result.body.stmts.truncate(0);

            for stmt in &expr_loop.body.stmts {
                let output_stmt = match shortcircuit_eligible_spawns_in_stmt(
                    stmt,
                    binding_is_unresolvable,
                    target_signatures_by_key,
                    should_return_value,
                )? {
                    RewriteResult::NoRewrite => stmt.clone(),
                    RewriteResult::Rewrite(rw) => rw,
                };

                expr_loop_result.body.stmts.push(output_stmt);
            }

            Ok(RewriteResult::Rewrite(syn::Expr::Loop(expr_loop_result)))
        }
        syn::Expr::Macro(ref expr_macro) => {
            if !structure::macro_stmt_is_spawn(&expr_macro.mac) {
                return Ok(RewriteResult::NoRewrite);
            }

            match shortcircuit_spawn_macro_if_eligible(
                &expr_macro.mac,
                binding_is_unresolvable,
                target_signatures_by_key,
                should_return_value,
            ) {
                Ok(RewriteResult::Rewrite(rw)) => {
                    Ok(RewriteResult::Rewrite(syn::Expr::Block(syn::ExprBlock {
                        attrs: vec![],
                        label: None,
                        block: rw,
                    })))
                }
                Ok(RewriteResult::NoRewrite) => Ok(RewriteResult::NoRewrite),
                Err(e) => return Err(e),
            }
        }
        _ => Ok(RewriteResult::NoRewrite),
    }
}

enum RewriteResult<V> {
    NoRewrite,
    Rewrite(V),
}

fn shortcircuit_spawn_macro_if_eligible(
    mac: &syn::Macro,
    binding_is_unresolvable: &HashMap<String, bool>,
    target_signatures_by_key: &HashMap<&String, syn::Signature>,
    should_return_value: bool,
) -> Result<RewriteResult<syn::Block>, syn::Error> {
    let args: structure::EpicArgs = mac.parse_body()?;

    let mut contains_unresolvable_arg = false;
    for arg in &args.positional_args {
        match arg {
            syn::Expr::Const(_) => continue,
            syn::Expr::Lit(_) => continue,
            syn::Expr::Path(ref expr_path) => {
                let identifier_segment = if expr_path.path.segments.len() == 1 {
                    expr_path.path.segments.first().unwrap()
                } else {
                    // NOTE for now we'll treat any member access in the argument list as
                    // unresolvable (safe).
                    contains_unresolvable_arg = true;
                    break;
                };

                let ident = identifier_segment.ident.to_string();
                if let Some(is_unresolvable) = binding_is_unresolvable.get(&ident) {
                    if !is_unresolvable {
                        continue;
                    }

                    contains_unresolvable_arg = true;
                    break;
                }
            }
            syn::Expr::Reference(_) => {
                // NOTE for now we consider this to be an unresolvable argument because the only
                // cases where we expect references to be passed as arguments is in case of a
                // `_sink` macro variant -- we can afford to be conservative and avoid
                // shortcircuiting these atm.
                contains_unresolvable_arg = true;
                break;
            }
            _ => {
                return Err(syn::Error::new(
                    arg.span(),
                    "unhandled argument type encountered during macro expansion",
                ))
            }
        }
    }

    if contains_unresolvable_arg {
        return Ok(RewriteResult::NoRewrite);
    }

    let target_function_str = args
        .target
        .into_token_stream()
        .to_string()
        .replace("\"", "");

    let target_function_signature = match target_signatures_by_key.get(&target_function_str) {
        Some(e) => e,
        None => return Ok(RewriteResult::NoRewrite),
    };

    let target_function = get_path_from_function_name(&target_function_str, false, false);
    let positional_args = args.positional_args;

    let block_output: syn::Block = syn::parse(match should_return_value {
        false => quote! {{
            #target_function(#(#positional_args),*);
        }}.into(),
        true => match target_function_signature.output {
            syn::ReturnType::Default => quote! {{
                let idx_before_spawn = object_lib::tls::get_last_unresolved_arg_idx().expect("failed to get idx");
                let ret = #target_function(#(#positional_args),*);
                let idx_after_spawn = object_lib::tls::get_last_unresolved_arg_idx().expect("failed to get idx");
                if idx_before_spawn != idx_after_spawn {
                    object_lib::tls::get_last_unresolved_arg().expect("failed to get last unresolved arg")
                }
            }}.into(),
            _ => quote! {{
                let idx_before_spawn = object_lib::tls::get_last_unresolved_arg_idx().expect("failed to get idx");
                let ret = #target_function(#(#positional_args),*);
                let idx_after_spawn = object_lib::tls::get_last_unresolved_arg_idx().expect("failed to get idx");

                if idx_before_spawn == idx_after_spawn {
                    ret
                } else {
                    object_lib::tls::get_last_unresolved_arg().expect("failed to get last unresolved arg")
                }
            }}.into()
        }
    }).expect("failed to parse macro replacement block");

    Ok(RewriteResult::Rewrite(block_output))
}

fn strip_type_from_pattern(pat: &syn::Pat) -> Result<syn::Pat, syn::Error> {
    match pat {
        syn::Pat::Ident(_) => Ok(pat.clone()),
        syn::Pat::Wild(_) => Ok(pat.clone()),
        syn::Pat::Type(ref pat_type) => Ok((*pat_type.pat).clone()),
        _ => panic!("unhandled pattern {:?}", pat),
    }
}
