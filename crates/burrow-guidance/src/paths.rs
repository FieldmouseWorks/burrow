//! The paths the guidance cites are there. Every local Markdown link, and
//! every path in backticks that starts at one of its repository's top-level
//! entries, has to name a file or directory that exists:
//!
//! - a link resolves from its document's directory (or the root, for `/x`)
//!   and may not leave the repository;
//! - a backticked path resolves from the repository's root or its
//!   document's directory, and counts as one only when its first part is an
//!   entry in one of them, so runtime paths (`/etc/...`, `~/.config/...`,
//!   `.slots/.lock`), branches and state files aren't taken for it (nor is
//!   a path whose whole top-level directory is gone: the price of that);
//! - one written as burrow's (``burrow's `modules/claude.nix` ``), and
//!   everything inside a house block, which is burrow's text wherever it's
//!   copied, resolves against burrow: the pinned input a consumer's gate
//!   runs with, or the tree itself in burrow's own;
//! - URLs, fenced code, and templates (`checks/NAME.nix`, `crates/CRATE`,
//!   `{a,b}`, `*`, `...`) are never checked.
//!
//! A finding names the file, line and target; the fix is the doc's, by
//! hand: nothing here edits it.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// The trees cited paths are looked for in.
pub struct Trees<'a> {
    /// The repository the guidance is in.
    pub own: &'a Path,
    /// The burrow it pins (itself, in burrow).
    pub burrow: &'a Path,
}

/// Which tree a path is cited from.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Repo {
    Own,
    Burrow,
}

/// A cited path that isn't there.
#[derive(Debug, PartialEq, Eq)]
pub struct Missing {
    pub file: String,
    pub line: usize,
    /// As the guidance wrote it.
    pub target: String,
    pub repo: Repo,
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repo = match self.repo {
            Repo::Own => "this repository",
            Repo::Burrow => "the pinned burrow",
        };
        write!(
            f,
            "{}:{}: cites {}, which {repo} doesn't have",
            self.file, self.line, self.target
        )
    }
}

/// The missing paths every Markdown file under `trees.own` cites.
pub fn check(trees: &Trees) -> Result<Vec<Missing>, String> {
    let mut files = Vec::new();
    crate::markdown(trees.own, &mut files)?;
    files.sort();
    let mut found = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = path
            .strip_prefix(trees.own)
            .unwrap_or(&path)
            .to_string_lossy();
        found.extend(check_text(&file, &text, trees));
    }
    Ok(found)
}

/// The missing paths one file's text cites, `file` being its path from the
/// root.
pub fn check_text(file: &str, text: &str, trees: &Trees) -> Vec<Missing> {
    let dir = Path::new(file).parent().unwrap_or(Path::new(""));
    let mut found = Vec::new();
    let mut fence: Option<&str> = None;
    let mut house = false;
    let mut prev = "";
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let trimmed = raw.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            fence = Some(open);
            continue;
        }
        if let Some(marker) = trimmed.strip_prefix("<!-- house:") {
            house = !marker.contains(" end -->");
            continue;
        }
        let mut miss = |target: &str, repo: Repo| {
            found.push(Missing {
                file: file.to_owned(),
                line,
                target: target.to_owned(),
                repo,
            });
        };
        let tree = |repo| match repo {
            Repo::Own => trees.own,
            Repo::Burrow => trees.burrow,
        };
        let (spans, prose) = code_spans(raw);
        for (at, code) in spans {
            let before = &raw[..at];
            let said = if before.trim().is_empty() {
                prev
            } else {
                before
            };
            let repo = if house || burrows(said) {
                Repo::Burrow
            } else {
                Repo::Own
            };
            // From the root, or from the document's own directory (a
            // crate's README naming `src/main.rs`); a path whose first part
            // is in neither isn't the repository's.
            let bases = [tree(repo).to_path_buf(), tree(repo).join(dir)];
            if let Some(path) = repo_path(code)
                && let Some(top) = path.split('/').next()
                && bases.iter().any(|b| b.join(top).exists())
                && !bases.iter().any(|b| exists(b, path))
            {
                miss(code, repo);
            }
        }
        let repo = if house { Repo::Burrow } else { Repo::Own };
        for target in links(&prose) {
            if let Some(path) = link_path(target) {
                let from = if path.starts_with('/') {
                    Path::new("")
                } else {
                    dir
                };
                let resolved = normalize(&from.join(path.trim_start_matches('/')));
                let there = resolved.is_some_and(|p| {
                    let p = p.to_string_lossy().into_owned();
                    let p = if path.ends_with('/') && !p.is_empty() {
                        p + "/"
                    } else {
                        p
                    };
                    p.is_empty() || exists(tree(repo), &p)
                });
                if !there {
                    miss(target, repo);
                }
            }
        }
        if !raw.trim().is_empty() {
            prev = raw;
        }
    }
    found
}

/// The path is in the tree: a directory when it ends in `/`.
fn exists(tree: &Path, path: &str) -> bool {
    let at = tree.join(path.trim_end_matches('/'));
    if path.ends_with('/') {
        at.is_dir()
    } else {
        at.exists()
    }
}

/// The text before a code span gives it to burrow: ``burrow's `x` ``, or
/// ``burrow's (`x` ``.
fn burrows(before: &str) -> bool {
    let before = before.trim_end();
    let before = before.strip_suffix('(').unwrap_or(before).trim_end();
    ["burrow's", "burrow’s", "Burrow's", "Burrow’s"]
        .iter()
        .any(|word| {
            before.strip_suffix(word).is_some_and(|rest| {
                !rest
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
            })
        })
}

/// The line's code spans, each with where it starts and what it holds,
/// and the line with each span blanked out.
fn code_spans(line: &str) -> (Vec<(usize, &str)>, String) {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut prose = line.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
        let body = i + run;
        let mut j = body;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let n = bytes[j..].iter().take_while(|&&b| b == b'`').count();
                if n == run {
                    close = Some(j);
                    break;
                }
                j += n;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                spans.push((i, line[body..end].trim()));
                prose[i..end + run].fill(b' ');
                i = end + run;
            }
            None => i = body,
        }
    }
    // Only ASCII was blanked, so it's still UTF-8.
    (spans, String::from_utf8(prose).unwrap_or_default())
}

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\]\(\s*<?([^)\s>]+)>?(?:\s+["'(][^)]*)?\)"#).expect("link pattern")
});
static DEFINITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{0,3}\[[^\]]+\]:\s*<?([^\s>]+)>?").expect("definition"));
static SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("scheme pattern"));
static SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_+.-]+(/[A-Za-z0-9_+.-]+)*/?$").expect("path pattern")
});

/// Every link target in the line, inline or a reference's definition.
fn links(prose: &str) -> Vec<&str> {
    let mut out: Vec<&str> = LINK
        .captures_iter(prose)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    out.extend(
        DEFINITION
            .captures(prose)
            .and_then(|c| c.get(1).map(|m| m.as_str())),
    );
    out
}

/// A link's local path, without its fragment or query: none for a URL, a
/// fragment of this page or a template.
fn link_path(target: &str) -> Option<String> {
    if SCHEME.is_match(target) || target.starts_with("//") || target.starts_with('#') {
        return None;
    }
    let path = target.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        return None;
    }
    let path = decode(path);
    (!template(&path)).then_some(path)
}

/// `%XX` escapes decoded, as a link's target has them.
fn decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = path
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A backticked path from its repository's root, if the code is one: a
/// line number after it (`lib/project.nix:42`) dropped, and none for
/// anything with a space, a scheme, a leading `/`, `~` or `$`, `..`, or
/// a template's marks.
fn repo_path(code: &str) -> Option<&str> {
    let mut path = code.strip_prefix("./").unwrap_or(code);
    if let Some((p, at)) = path.rsplit_once(':')
        && !at.is_empty()
        && at.chars().all(|c| c.is_ascii_digit() || c == '-')
    {
        path = p;
    }
    let shaped = path.contains('/')
        && SHAPE.is_match(path)
        && !path.split('/').any(|s| s == "." || s == "..");
    (shaped && !template(path)).then_some(path)
}

/// Files named in capitals by convention, which aren't placeholders.
const CAPITALS: &[&str] = &[
    "AGENTS",
    "AUTHORS",
    "CHANGELOG",
    "CLAUDE",
    "CODEOWNERS",
    "CODE_OF_CONDUCT",
    "CONTRIBUTING",
    "COPYING",
    "FUNDING",
    "HACKING",
    "INSTALL",
    "ISSUE_TEMPLATE",
    "LICENSE",
    "MAINTAINERS",
    "MEMORY",
    "NOTICE",
    "PULL_REQUEST_TEMPLATE",
    "README",
    "SECURITY",
    "SKILL",
    "TODO",
];

/// A path that stands for many: `{a,b}`, a glob, `...`, or a placeholder
/// word in capitals (`NAME`, `CRATE`, `OWNER_REPO-N`).
fn template(path: &str) -> bool {
    path.contains(['{', '}', '*', '?', '<', '>', '[', ']', '$', '@', '|', ' '])
        || path.contains("...")
        || path.split(['/', '.', '-']).any(|word| {
            word.chars().any(|c| c.is_ascii_uppercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && !CAPITALS.contains(&word)
        })
}

/// The path with `.` and `..` folded away; none if it leaves the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
