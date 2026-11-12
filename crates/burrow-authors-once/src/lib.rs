//! The one-authors-list rule: the authorized authors are declared once,
//! `burrow.authors` (modules/common.nix), and every door reads that option.
//! No other module declares an authors-like option or takes `authors` from
//! anywhere else, in any spelling: a second option, a literal list, an
//! alias, a qualified or quoted attribute, a binding over several lines.
//!
//! A module's bindings and reads are taken from its Nix syntax tree, so
//! comments and ordinary string data do not count; interpolations are code.
//! Every name that mentions authors must be one of the shared forms, plus the
//! few a module is named for. The autoland modules' own `authors` are PR
//! author logins for the steward, not the doors' accounts: each of their
//! forms is listed, so a door payload in the same file is still refused.

mod syntax;

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
const OWN: [(&str, &[(&str, &str)]); 4] = [
    // Where `burrow.authors` and its type are declared, and nothing else.
    (
        "modules/common.nix",
        &[
            (
                "imports.options.burrow",
                "authors = mkOption { type = types.listOf ( import ../lib/github-author.nix { inherit lib; } )*",
            ),
            (
                "imports.options.burrow.authors.type",
                "../lib/github-author.nix",
            ),
        ],
    ),
    (
        "modules/autoland.nix",
        &[
            ("settings", "inherit (cfg) authors"),
            (
                "upkeepConfig",
                "inherit (settings) stateDir owner proj projectsDir authors bots app jobTimeout renovate",
            ),
            (
                "options.burrow.autoland",
                "authors = mkOption { type = types.listOf types.str*",
            ),
            (
                "config",
                "burrow.authors = [ { login = \"${cfg.app.slug}[bot]\"; id = cfg.app.accountId; } ]",
            ),
        ],
    ),
    (
        "modules/autoland/shadow.nix",
        &[
            ("flowConfig", "trusted_authors = cfg.authors*"),
            ("flowConfig.trusted_authors", "cfg.authors"),
            ("flowConfig.settings", "review_authors = [ stewardLogin ]"),
            ("flowConfig.settings", "verdict_authors = [ stewardLogin ]*"),
            ("flowConfig.settings.people", "cfg.authors"),
        ],
    ),
    (
        "modules/watchdog/degradations.nix",
        &[
            ("settings", "inherit (cfg) repos prThreshold authors"),
            (
                "options.burrow.watchdog.degradations",
                "authors = mkOption { type = types.listOf types.str*",
            ),
            (
                "options.burrow.watchdog.degradations.authors.default",
                "config.burrow.autoland.authors",
            ),
            ("config.assertions.assertion", "cfg.authors"),
        ],
    ),
];

/// Whether `form` is the allowed one: all of it, or, for an entry ending in
/// `*`, its beginning (an option's declaration, whose body is its own).
fn allows(allowed: &str, form: &str) -> bool {
    match allowed.strip_suffix('*') {
        Some(head) => form.starts_with(&syntax::normalized(head)),
        None => form == syntax::normalized(allowed),
    }
}

fn mentions(name: &str) -> bool {
    name.to_lowercase().contains("authors")
}

/// What is wrong with these modules, one line each; empty when the rule holds.
pub fn scan(files: &[File]) -> Vec<String> {
    let mut found = Vec::new();
    for file in files {
        let parsed = rnix::Root::parse(&file.text);
        if !parsed.errors().is_empty() {
            found.push(format!("{}: invalid Nix: {:?}", file.path, parsed.errors()));
            continue;
        }
        let own = OWN
            .iter()
            .find(|(p, _)| *p == file.path)
            .map_or(&[][..], |(_, forms)| forms);
        for (what, scope, form) in syntax::forms(&parsed.syntax()) {
            let ok = SHARED.iter().any(|a| allows(a, &form))
                || own.iter().any(|(at, a)| {
                    scope
                        .iter()
                        .map(String::as_str)
                        .eq(at.split('.').filter(|s| !s.is_empty()))
                        && allows(a, &form)
                });
            if !ok {
                found.push(format!(
                    "{}: {what}: `{form}` in `{}` (the authors are burrow.authors)",
                    file.path,
                    scope.join(".")
                ));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests;
