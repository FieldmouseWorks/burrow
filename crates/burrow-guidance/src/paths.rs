//! The paths the guidance cites are there. Every local Markdown link, and
//! every path in backticks that starts at one of its repository's top-level
//! entries, has to name a file or directory that exists:
//!
//! - a link (inline, an image, or a reference's definition, as CommonMark
//!   reads them) resolves from its document's directory (or the root, for
//!   `/x`), and names its file literally: spaces and punctuation are part
//!   of the name;
//! - a backticked path resolves from the repository's root or its
//!   document's directory, and counts as one only when its first part is an
//!   entry in one of them, so runtime paths (`/etc/...`, `~/.config/...`,
//!   `.slots/.lock`), branches and state files aren't taken for it (nor is
//!   a path whose whole top-level directory is gone: the price of that);
//! - one written as burrow's (``burrow's `modules/claude.nix` ``), and
//!   everything inside a house block, which is burrow's text wherever it's
//!   copied, resolves against burrow: the pinned input a consumer's gate
//!   runs with, or the tree itself in burrow's own;
//! - every target stays in its tree, symlinks resolved: one that leaves it
//!   lexically or through a symlink, or ends at a broken one, fails;
//! - URLs, code blocks (fenced or indented, in a list or a quote too), and
//!   placeholders (`checks/NAME.nix`, `crates/CRATE`; in backticks also
//!   `{a,b}`, `*`, `...`) are never checked.
//!
//! A finding names the file, line and target; the fix is the doc's, by
//! hand: nothing here edits it.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
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
/// root. The text is read as CommonMark (pulldown-cmark), so a link's
/// destination is all of it (parentheses, escapes, line breaks, `<...>`),
/// code spans are what the parser takes for one, and fenced or indented
/// code, in a list or quote too, is never read.
pub fn check_text(file: &str, text: &str, trees: &Trees) -> Vec<Missing> {
    let dir = Path::new(file).parent().unwrap_or(Path::new(""));
    let roots = Roots::new(trees);
    let lines = Lines::new(text);
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let mut events = Parser::new_ext(text, options).into_offset_iter();
    // Where each house block opens (true) and closes (false).
    let mut house: Vec<(usize, bool)> = Vec::new();
    let mut found: Vec<(usize, Missing)> = Vec::new();
    let in_house = |house: &[(usize, bool)], at: usize| {
        house
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .is_some_and(|(_, open)| *open)
    };
    let mut miss = |at: usize, target: &str, repo: Repo| {
        found.push((
            at,
            Missing {
                file: file.to_owned(),
                line: lines.of(at),
                target: target.to_owned(),
                repo,
            },
        ));
    };
    for (event, range) in events.by_ref() {
        match event {
            Event::Html(html) | Event::InlineHtml(html) => {
                for l in html.lines() {
                    if let Some(marker) = l.trim_start().strip_prefix("<!-- house:") {
                        house.push((range.start, !marker.contains(" end -->")));
                    }
                }
            }
            Event::Code(code) => {
                let code = code.trim();
                let repo = if in_house(&house, range.start) || burrows(lines.before(range.start)) {
                    Repo::Burrow
                } else {
                    Repo::Own
                };
                // From the root, or from the document's own directory (a
                // crate's README naming `src/main.rs`); a path whose first
                // part is in neither isn't the repository's.
                let bases = [PathBuf::new(), dir.to_path_buf()];
                if let Some(path) = repo_path(code)
                    && let Some(top) = path.split('/').next()
                    && bases.iter().any(|b| roots.has_entry(repo, &b.join(top)))
                    && !bases
                        .iter()
                        .any(|b| roots.exists(repo, &b.join(path), path.ends_with('/')))
                {
                    miss(range.start, code, repo);
                }
            }
            // A reference's link is checked at its definition, below; an
            // autolink is a URL.
            Event::Start(
                Tag::Link {
                    link_type: LinkType::Inline,
                    dest_url,
                    ..
                }
                | Tag::Image {
                    link_type: LinkType::Inline,
                    dest_url,
                    ..
                },
            ) => {
                let repo = if in_house(&house, range.start) {
                    Repo::Burrow
                } else {
                    Repo::Own
                };
                if !roots.links_to(repo, dir, &dest_url) {
                    miss(range.start, &dest_url, repo);
                }
            }
            _ => {}
        }
    }
    for (_, def) in events.reference_definitions().iter() {
        let at = def.span.start;
        let repo = if in_house(&house, at) {
            Repo::Burrow
        } else {
            Repo::Own
        };
        if !roots.links_to(repo, dir, &def.dest) {
            miss(at, &def.dest, repo);
        }
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, m)| m).collect()
}

/// The trees as the filesystem has them, symlinks resolved, so a target is
/// in its tree only when what it resolves to is.
struct Roots<'a> {
    trees: &'a Trees<'a>,
    own: PathBuf,
    burrow: PathBuf,
}

impl<'a> Roots<'a> {
    fn new(trees: &'a Trees<'a>) -> Self {
        // A root that isn't there has nothing in it: every target misses.
        let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        Roots {
            trees,
            own: real(trees.own),
            burrow: real(trees.burrow),
        }
    }

    fn tree(&self, repo: Repo) -> (&Path, &Path) {
        match repo {
            Repo::Own => (self.trees.own, &self.own),
            Repo::Burrow => (self.trees.burrow, &self.burrow),
        }
    }

    /// `rel` is an entry of the tree, whatever it points at: a symlink out
    /// of it, or a broken one, still makes what's cited through it the
    /// repository's.
    fn has_entry(&self, repo: Repo, rel: &Path) -> bool {
        self.tree(repo).0.join(rel).symlink_metadata().is_ok()
    }

    /// `rel` (no `..` in it) names a file or directory, a directory when
    /// `dir`, that resolves to inside the tree.
    fn exists(&self, repo: Repo, rel: &Path, dir: bool) -> bool {
        let (tree, real) = self.tree(repo);
        fs::canonicalize(tree.join(rel))
            .is_ok_and(|at| at.starts_with(real) && (!dir || at.is_dir()))
    }

    /// A link's destination, from a document in `dir`, is there: a URL, a
    /// fragment of this page or a template is never checked.
    fn links_to(&self, repo: Repo, dir: &Path, target: &str) -> bool {
        let Some(path) = link_path(target) else {
            return true;
        };
        let from = if path.starts_with('/') {
            Path::new("")
        } else {
            dir
        };
        match normalize(&from.join(path.trim_start_matches('/'))) {
            None => false,
            Some(p) if p.as_os_str().is_empty() => true,
            Some(p) => self.exists(repo, &p, path.ends_with('/')),
        }
    }
}

/// Byte offsets to line numbers, and what's before an offset on its line.
struct Lines<'a> {
    text: &'a str,
    /// Where each line starts.
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Lines { text, starts }
    }

    /// The 1-based line `at` is on.
    fn of(&self, at: usize) -> usize {
        self.starts.partition_point(|&s| s <= at)
    }

    /// The text before `at` on its line, past any quote markers; the last
    /// line before it with any text when there's none (``burrow's`` ending
    /// one line, its path starting the next).
    fn before(&self, at: usize) -> &'a str {
        let line = self.of(at) - 1;
        let here = self.text[self.starts[line]..at]
            .trim_start_matches(|c: char| c == '>' || c.is_whitespace());
        if !here.trim().is_empty() {
            return here;
        }
        (0..line)
            .rev()
            .map(|l| {
                let end = self.starts[l + 1].saturating_sub(1);
                &self.text[self.starts[l]..end]
            })
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
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

static SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("scheme pattern"));
static SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_+.-]+(/[A-Za-z0-9_+.-]+)*/?$").expect("path pattern")
});

/// A link's local path, without its fragment or query and with its `%XX`
/// escapes decoded: none for a URL, a fragment of this page or a
/// placeholder (`NAME.md`). Anything else in it is the file's name, spaces
/// and punctuation included.
fn link_path(target: &str) -> Option<String> {
    if SCHEME.is_match(target) || target.starts_with("//") || target.starts_with('#') {
        return None;
    }
    let path = target.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() || placeholder(path) {
        return None;
    }
    Some(decode(path))
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

/// A backticked path that stands for many: `{a,b}`, a glob, `...`, or a
/// placeholder.
fn template(path: &str) -> bool {
    path.contains(['{', '}', '*', '?', '<', '>', '[', ']', '$', '@', '|', ' '])
        || path.contains("...")
        || placeholder(path)
}

/// A placeholder word in capitals (`NAME`, `CRATE`, `OWNER_REPO-N`), but
/// not a file named in them by convention.
fn placeholder(path: &str) -> bool {
    path.split(['/', '.', '-']).any(|word| {
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
