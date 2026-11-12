//! Nix syntax owns the binding context, quoted attributes and interpolation
//! boundaries. No token or statement-boundary heuristics decide exemptions.

use rnix::{SyntaxNode, ast};
use rowan::ast::AstNode;

use super::mentions;

pub fn normalized(text: &str) -> String {
    rnix::tokenize(text)
        .filter(|(kind, _)| !kind.is_trivia())
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Static string parts retain their names. Unknown interpolation values carry
/// a marker so they cannot impersonate any literal owning-path component.
fn string_name(string: ast::Str) -> String {
    string
        .normalized_parts()
        .into_iter()
        .map(|part| match part {
            ast::InterpolPart::Literal(text) => text,
            ast::InterpolPart::Interpolation(interpol) => {
                interpol.expr().map(expr_name).unwrap_or_default()
            }
        })
        .collect()
}

fn expr_name(expr: ast::Expr) -> String {
    match expr {
        ast::Expr::Str(string) => string_name(string),
        ast::Expr::Paren(paren) => paren.expr().map(expr_name).unwrap_or_default(),
        _ => "${dynamic}".into(),
    }
}

fn attr_name(attr: ast::Attr) -> String {
    match attr {
        ast::Attr::Ident(id) => id.to_string(),
        ast::Attr::Str(string) => string_name(string),
        ast::Attr::Dynamic(dynamic) => dynamic.expr().map(expr_name).unwrap_or_default(),
    }
}

fn names(path: ast::Attrpath) -> Vec<String> {
    path.attrs().map(attr_name).collect()
}

/// Every enclosing binding is an ancestor regardless of the expression in
/// its value: let/in, with, assert, calls, lambdas, lists, interpolations, etc.
/// Keep components separate: a quoted name containing a dot is one name.
fn scope(node: &SyntaxNode) -> Vec<String> {
    let mut ancestors: Vec<_> = node.ancestors().skip(1).collect();
    ancestors.reverse();
    ancestors
        .into_iter()
        .filter_map(ast::AttrpathValue::cast)
        .filter_map(|binding| binding.attrpath())
        .flat_map(names)
        .collect()
}

fn form(node: &SyntaxNode) -> String {
    let text = normalized(&node.to_string());
    text.strip_suffix(" ;").unwrap_or(&text).to_owned()
}

/// How many arguments remain before this importer receives its path. Recognize
/// the Nix builtins structurally, including parenthesized and quoted forms.
fn import_arity(expr: ast::Expr) -> Option<usize> {
    match expr {
        ast::Expr::Paren(paren) => import_arity(paren.expr()?),
        ast::Expr::Ident(id) => match id.to_string().as_str() {
            "import" => Some(1),
            "scopedImport" => Some(2),
            _ => None,
        },
        ast::Expr::Select(select) if select.expr()?.to_string() == "builtins" => {
            match names(select.attrpath()?).as_slice() {
                [name] if name == "import" => Some(1),
                [name] if name == "scopedImport" => Some(2),
                _ => None,
            }
        }
        ast::Expr::Apply(apply) => import_arity(apply.lambda()?)?.checked_sub(1),
        _ => None,
    }
}

fn is_import_operand(node: &SyntaxNode) -> bool {
    node.ancestors()
        .skip(1)
        .filter_map(ast::Apply::cast)
        .any(|apply| {
            let Some(argument) = apply.argument() else {
                return false;
            };
            let Some(lambda) = apply.lambda() else {
                return false;
            };
            argument
                .syntax()
                .text_range()
                .contains_range(node.text_range())
                && import_arity(lambda) == Some(1)
        })
}

pub fn forms(root: &SyntaxNode) -> Vec<(&'static str, Vec<String>, String)> {
    let mut out = Vec::new();
    for node in root.descendants() {
        if let Some(binding) = ast::AttrpathValue::cast(node.clone()) {
            if binding
                .attrpath()
                .is_some_and(|p| names(p).iter().any(|n| mentions(n)))
            {
                out.push(("declares or sets authors", scope(&node), form(&node)));
            }
        } else if let Some(inherit) = ast::Inherit::cast(node.clone()) {
            if inherit.attrs().map(attr_name).any(|n| mentions(&n)) {
                out.push(("inherits authors", scope(&node), form(&node)));
            }
        } else if let Some(select) = ast::Select::cast(node.clone()) {
            if let Some(path) = select
                .attrpath()
                .filter(|p| names(p.clone()).iter().any(|n| mentions(n)))
            {
                // `or` is a default expression, not part of the read's path.
                let read = format!("{} . {}", select.expr().unwrap(), path);
                out.push(("reads authors", scope(&node), normalized(&read)));
            }
        } else if let Some(id) = ast::Ident::cast(node.clone()) {
            // Attribute paths and inherited names were checked as whole forms.
            if mentions(&id.to_string())
                && !node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        rnix::SyntaxKind::NODE_ATTRPATH | rnix::SyntaxKind::NODE_INHERIT
                    )
                })
            {
                out.push(("reads authors", scope(&node), form(&node)));
            }
        } else if ast::Path::cast(node.clone()).is_some()
            && node.to_string().contains("github-author")
        {
            out.push(("takes the authors' type", scope(&node), form(&node)));
        } else if let Some(string) = ast::Str::cast(node.clone())
            && is_import_operand(&node)
            && string_name(string).contains("github-author")
        {
            out.push(("takes the authors' type", scope(&node), form(&node)));
        }
    }
    out
}
