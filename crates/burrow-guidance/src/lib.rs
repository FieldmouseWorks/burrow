//! The guidance (every Markdown file in a repo) names a fact only where it's
//! owned. Each [`Fact`] is a pattern and the one file that owns it; a
//! sentence anywhere else that matches it is a finding, to be replaced by a
//! link to the owner.
//!
//! The table is [`facts`]: today the settings, from burrow-settings' schema
//! (an option named together with a value, or the file a live one is kept
//! in, belongs to the generated reference). Another owned fact is one more
//! source chained there.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

pub mod settings;

/// One owned fact.
pub struct Fact {
    /// What it is, as a finding names it.
    pub what: String,
    /// The file that owns it, from the repo's root: the one place it may be
    /// stated, and where the others link.
    pub owner: &'static str,
    /// A sentence that matches this names the fact...
    pub names: Regex,
    /// ...and, when this is set, states it only if this matches too, near
    /// the name.
    pub with: Option<Near>,
}

/// A pattern that has to match within `chars` of the name.
pub struct Near {
    pub pattern: Regex,
    pub chars: usize,
}

impl Fact {
    fn stated_in(&self, sentence: &str) -> bool {
        self.names.find_iter(sentence).any(|name| {
            self.with.as_ref().is_none_or(|near| {
                near.pattern.find_iter(sentence).any(|w| {
                    w.start() <= name.end() + near.chars && name.start() <= w.end() + near.chars
                })
            })
        })
    }
}

/// Every owned fact the guidance is held to.
pub fn facts() -> Vec<Fact> {
    settings::facts()
}

/// A fact stated outside its owner.
#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub what: String,
    pub owner: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: states {}; link {} instead",
            self.file, self.line, self.what, self.owner
        )
    }
}

/// The findings in one file's text, `file` being its path from the root.
pub fn check_text(file: &str, text: &str, facts: &[Fact]) -> Vec<Finding> {
    let mut found = Vec::new();
    for (line, sentence) in sentences(text) {
        for fact in facts.iter().filter(|f| f.owner != file) {
            if fact.stated_in(&sentence) {
                found.push(Finding {
                    file: file.to_owned(),
                    line,
                    what: fact.what.clone(),
                    owner: fact.owner,
                });
            }
        }
    }
    found
}

/// The findings in every Markdown file under `root`.
pub fn check(root: &Path, facts: &[Fact]) -> Result<Vec<Finding>, String> {
    let mut files = Vec::new();
    markdown(root, &mut files)?;
    files.sort();
    let mut found = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        found.extend(check_text(&file, &text, facts));
    }
    Ok(found)
}

/// Every `.md` file under `dir`, leaving out what isn't the repo's own
/// (`.git`, build output).
fn markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            if !matches!(name.to_str(), Some(".git" | "target" | "result")) {
                markdown(&path, out)?;
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// The text's sentences, each with the line it starts on: a paragraph is
/// split after `.`, `;`, `?` or `!` and a space; a list item, a table
/// row, a heading and each line of a code block stand alone.
/// A colon keeps an option and its value in the same statement.
fn sentences(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut open = Paragraph::default();
    let mut fenced = false;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            open.split(&mut out);
            fenced = !fenced;
            continue;
        }
        let alone = fenced || trimmed.starts_with('|') || trimmed.starts_with('#');
        let starts = trimmed.is_empty()
            || alone
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed
                .split_once(". ")
                .is_some_and(|(n, _)| n.parse::<u32>().is_ok());
        if starts {
            open.split(&mut out);
        }
        if alone {
            out.push((line, raw.to_owned()));
        } else if !trimmed.is_empty() {
            open.push(line, trimmed);
        }
    }
    open.split(&mut out);
    out
}

/// A paragraph's lines joined, and where each starts in the joined text.
#[derive(Default)]
struct Paragraph {
    text: String,
    lines: Vec<(usize, usize)>,
}

impl Paragraph {
    fn push(&mut self, line: usize, text: &str) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.lines.push((self.text.len(), line));
        self.text.push_str(text);
    }

    /// The line the text from `at` starts on.
    fn line(&self, at: usize) -> usize {
        self.lines
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .map_or(0, |(_, line)| *line)
    }

    /// Its sentences into `out`, leaving it empty.
    fn split(&mut self, out: &mut Vec<(usize, String)>) {
        let text = std::mem::take(&mut self.text);
        let bytes = text.as_bytes();
        let mut start = 0;
        for (i, b) in bytes.iter().enumerate() {
            if matches!(b, b'.' | b';' | b'?' | b'!') && bytes.get(i + 1) == Some(&b' ') {
                out.push((self.line(start), text[start..=i].to_owned()));
                start = i + 2;
            }
        }
        if start < text.len() {
            out.push((self.line(start), text[start..].to_owned()));
        }
        self.lines.clear();
    }
}

#[cfg(test)]
mod tests;
