//! The one-authors-list rule: the authorized authors are declared once,
//! `burrow.authors` (modules/common.nix), and every door reads that option.
//! No other module declares an authors-like option or takes `authors` from
//! anywhere else, in any spelling: a second option, a literal list, an
//! alias, a qualified or quoted attribute, a binding over several lines.
//!
//! A module's names are read from its tokens (`lex`), so comments and
//! strings don't count. Every name that mentions authors must be one of
//! the forms in `allowed`, which is the same for every module, plus the
//! few a module is named for. The autoland modules' own `authors` are PR
//! author logins for the steward, not the doors' accounts: each of their
//! forms is listed, so a door payload in the same file is still refused.

mod lex;

use lex::{Tok, lex};

/// A module's file, relative to the tree's root, and its text.
pub struct File {
    pub path: String,
    pub text: String,
}

/// What reads `config.burrow.authors`, in any module.
const SHARED: [&str; 3] = [
    "inherit (config.burrow) authors",
    "authors = config.burrow.authors",
    "config.burrow.authors",
];

/// The forms only these modules may use: where the list is declared, and the
/// steward's own PR authors. A declaration carries the path it sits under, so
/// the same words under another option are refused.
const OWN: [(&str, &[&str]); 4] = [
    // Where `burrow.authors` and its type are declared, and nothing else.
    (
        "modules/common.nix",
        &[
            "options.burrow.authors = mkOption { type = types.listOf ( import ../lib/github-author.nix { inherit lib; } )*",
            "../lib/github-author.nix",
        ],
    ),
    (
        "modules/autoland.nix",
        &[
            "inherit (cfg) authors",
            "inherit (settings) stateDir owner proj projectsDir authors bots app jobTimeout renovate",
            "options.burrow.autoland.authors = mkOption { type = types.listOf types.str*",
            "burrow.authors = [ { login = \"${cfg.app.slug}[bot]\"; id = cfg.app.accountId; } ]",
            "cfg.authors",
        ],
    ),
    (
        "modules/autoland/shadow.nix",
        &[
            "trusted_authors = cfg.authors*",
            "review_authors = [ stewardLogin ]",
            "verdict_authors = [ stewardLogin ]*",
            "cfg.authors",
        ],
    ),
    (
        "modules/watchdog/degradations.nix",
        &[
            "inherit (cfg) repos prThreshold authors",
            "options.burrow.watchdog.degradations.authors = mkOption { type = types.listOf types.str*",
            "config.burrow.autoland.authors",
            "cfg.authors",
        ],
    ),
];

/// Whether `form` is the allowed one: all of it, or, for an entry ending in
/// `*`, its beginning (an option's declaration, whose body is its own).
fn allows(allowed: &str, form: &str) -> bool {
    match allowed.strip_suffix('*') {
        Some(head) => form.starts_with(&spaced(&lex(head))),
        None => form == spaced(&lex(allowed)),
    }
}

fn spaced(toks: &[Tok]) -> String {
    let parts: Vec<String> = toks
        .iter()
        .map(|t| match t {
            Tok::Word(w) => w.clone(),
            Tok::Punct(c) => c.to_string(),
        })
        .collect();
    parts.join(" ")
}

fn mentions(name: &str) -> bool {
    name.split('.')
        .any(|seg| seg.trim_matches('"').to_lowercase().contains("authors"))
}

/// What is wrong with these modules, one line each; empty when the rule holds.
pub fn scan(files: &[File]) -> Vec<String> {
    let mut found = Vec::new();
    for file in files {
        let own = OWN
            .iter()
            .find(|(p, _)| *p == file.path)
            .map_or(&[][..], |(_, forms)| forms);
        for (what, form) in forms(&lex(&file.text)) {
            let ok = SHARED.iter().chain(own).any(|a| allows(a, &form));
            if !ok {
                found.push(format!(
                    "{}: {what}: `{form}` (the authors are burrow.authors)",
                    file.path
                ));
            }
        }
    }
    found
}

/// Every place the tokens name authors or the type that describes one, as
/// what it is and the tokens around it (the statement from its start, to
/// its `;` when it is a binding), spaced out.
fn forms(toks: &[Tok]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        let Tok::Word(w) = tok else { continue };
        if w.contains("github-author") {
            out.push(("takes the authors' type", w.clone()));
        } else if w.contains('/') || !mentions(w) {
            continue;
        } else if is_bind(toks.get(i + 1)) {
            out.push(("declares or sets authors", binding(toks, i)));
        } else if inherited(toks, i) {
            out.push(("inherits authors", statement(toks, inherit_start(toks, i))));
        } else {
            out.push(("reads authors", w.clone()));
        }
    }
    out
}

/// A binding's statement; a declaration (`mkOption`) is named by every
/// attribute it sits under (`options.burrow.x.authors`), so where it is
/// declared is part of what is allowed.
fn binding(toks: &[Tok], i: usize) -> String {
    let text = statement(toks, i);
    let declares = matches!(toks.get(i + 2), Some(Tok::Word(w)) if w.ends_with("mkOption"));
    if !declares {
        return text;
    }
    let mut stack: Vec<String> = Vec::new();
    for n in 0..i {
        match &toks[n] {
            Tok::Punct('{' | '[' | '(') => {
                let name = match (
                    n.checked_sub(2).and_then(|k| toks.get(k)),
                    n.checked_sub(1).and_then(|k| toks.get(k)),
                ) {
                    (Some(Tok::Word(w)), Some(Tok::Punct('='))) if toks[n] == Tok::Punct('{') => {
                        w.clone()
                    }
                    _ => String::new(),
                };
                stack.push(name);
            }
            Tok::Punct('}' | ']' | ')') => {
                stack.pop();
            }
            _ => {}
        }
    }
    let Tok::Word(w) = &toks[i] else { return text };
    let path: Vec<&str> = stack
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .chain([w.as_str()])
        .collect();
    let rest = text.strip_prefix(w.as_str()).unwrap_or(&text);
    format!("{}{rest}", path.join("."))
}

fn is_bind(tok: Option<&Tok>) -> bool {
    tok == Some(&Tok::Punct('='))
}

/// Whether the word at `i` is among the names of an `inherit ... ;`.
fn inherited(toks: &[Tok], i: usize) -> bool {
    inherit_start(toks, i) != i
}

fn inherit_start(toks: &[Tok], i: usize) -> usize {
    for j in (0..i).rev() {
        match &toks[j] {
            Tok::Punct(';' | '{' | '}' | '[' | '=') => return i,
            Tok::Word(w) if w == "inherit" => return j,
            _ => {}
        }
    }
    i
}

/// The tokens from `start` to the end of the statement, spaced out: for a
/// binding, to its `;` at the same depth, so a value over several lines is
/// one string.
fn statement(toks: &[Tok], start: usize) -> String {
    let mut depth = 0i32;
    let mut end = toks.len();
    for (n, tok) in toks.iter().enumerate().skip(start) {
        match tok {
            Tok::Punct('{' | '[' | '(') => depth += 1,
            Tok::Punct('}' | ']' | ')') => {
                depth -= 1;
                if depth < 0 {
                    end = n;
                    break;
                }
            }
            Tok::Punct(';') if depth == 0 => {
                end = n;
                break;
            }
            _ => {}
        }
    }
    spaced(&toks[start..end])
}

#[cfg(test)]
mod tests;
