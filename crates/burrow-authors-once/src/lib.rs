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

/// Where `burrow.authors` and its type are declared; scanned by no one.
const HOME: [&str; 2] = ["modules/common.nix", "lib/github-author.nix"];

/// What reads `config.burrow.authors`, in any module.
const SHARED: [&str; 3] = [
    "inherit (config.burrow) authors",
    "authors = config.burrow.authors",
    "config.burrow.authors",
];

/// The forms only these modules may use: the steward's own PR authors.
const OWN: [(&str, &[&str]); 3] = [
    (
        "modules/autoland.nix",
        &[
            "inherit (cfg) authors",
            "inherit (settings) stateDir owner proj projectsDir authors bots app jobTimeout renovate",
            "authors = mkOption { type = types.listOf types.str*",
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
            "authors = mkOption { type = types.listOf types.str*",
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
    for file in files.iter().filter(|f| !HOME.contains(&f.path.as_str())) {
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
            out.push(("declares or sets authors", statement(toks, i)));
        } else if inherited(toks, i) {
            out.push(("inherits authors", statement(toks, inherit_start(toks, i))));
        } else {
            out.push(("reads authors", w.clone()));
        }
    }
    out
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
