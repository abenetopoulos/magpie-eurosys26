use quote::{quote, ToTokens};
use syn;
use syn::spanned::Spanned;

type KeyExprPair = (String, syn::Expr);

fn process_expr(expr: &syn::Expr) -> Result<KeyExprPair, syn::Error> {
    match expr {
        syn::Expr::Field(expr_field) => Ok((
            expr_field.base.to_token_stream().to_string(),
            syn::Expr::Field(expr_field.clone()),
        )),
        syn::Expr::Unary(expr_field) => Ok((
            expr_field.expr.to_token_stream().to_string(),
            (*expr_field.expr).clone(),
        )),
        _ => Err(syn::Error::new(
            expr.span(),
            format!("Unhandled expression: {}", expr.to_token_stream()),
        )),
    }
}

fn process_expr_assign(expr_assign: &syn::ExprAssign) -> Result<KeyExprPair, syn::Error> {
    process_expr(&*expr_assign.left)
}

// TODO handle bit-shift operators
fn process_expr_binary(expr_binary: &syn::ExprBinary) -> Result<KeyExprPair, syn::Error> {
    match expr_binary.op {
        syn::BinOp::AddAssign(_)
        | syn::BinOp::SubAssign(_)
        | syn::BinOp::MulAssign(_)
        | syn::BinOp::DivAssign(_)
        | syn::BinOp::RemAssign(_) => process_expr(&*expr_binary.left),
        _ => {
            let operator_token_stream = expr_binary.op.to_token_stream();
            return Err(syn::Error::new(
                expr_binary.span(),
                format!(
                    "unsupported binary operator: {}",
                    quote! { #operator_token_stream }
                ),
            ));
        }
    }
}

fn process_statement(stmt: &syn::Stmt) -> Result<Option<KeyExprPair>, syn::Error> {
    let process_res = match stmt {
        syn::Stmt::Expr(expr, _) => match expr {
            syn::Expr::Assign(expr_assign) => match &*expr_assign.left {
                syn::Expr::Index(_) => None,
                _ => Some(process_expr_assign(expr_assign)),
            },
            syn::Expr::Binary(expr_binary) => Some(process_expr_binary(expr_binary)),
            _ => None,
        },
        _ => None,
    };

    match process_res {
        None => Ok(None),
        Some(e) => match e {
            Ok(r) => Ok(Some(r)),
            Err(e) => Err(e),
        },
    }
}

pub(crate) fn extract_effects(block: &Box<syn::Block>) -> Result<Vec<KeyExprPair>, syn::Error> {
    let mut res = vec![];

    for stmt in &block.stmts {
        match process_statement(stmt) {
            Ok(me) => match me {
                Some(e) => res.push(e),
                None => (),
            },
            Err(e) => return Err(e),
        }
    }

    Ok(res)
}
