//! What a PR's diff holds that the rules read, from the comparison of its
//! base and head: two commits, which never change.

use std::collections::HashMap;

use serde_json::Value;

use crate::github::GitHub;
use crate::rules::{self, Language};

/// The most files GitHub lists in a comparison. Past that, what it leaves
/// out could break any rule, so a PR that long needs every line.
pub const LISTED: usize = 300;

#[derive(Debug, Default)]
pub struct Diff {
    /// How many files GitHub listed.
    pub listed: usize,
    /// Files that become Python: added, copied, or Python where they
    /// weren't, whatever their lines.
    pub new_python: Vec<String>,
    /// Lines added to Python and shell, net of every line taken out of
    /// either, files that stop being either included. A new shell script
    /// counts its lines here, so glue passes and a feature in bash doesn't.
    pub growth: i64,
    /// Code files the PR leaves longer than it found them and past the limit
    /// at its head, with their length there.
    pub big: Vec<(String, usize)>,
    /// Files with a line the PR adds that sets up a timer, outside Markdown
    /// and comments.
    pub timers: Vec<String>,
    /// Files with a line the PR adds that names a model.
    pub models: Vec<String>,
}

/// Files' content as GitHub has it, each read once.
struct Contents<'a> {
    gh: &'a dyn GitHub,
    read: HashMap<(String, String), Vec<u8>>,
}

impl Contents<'_> {
    fn get(&mut self, path: &str, at: &str) -> Result<&[u8], String> {
        let key = (path.to_owned(), at.to_owned());
        if !self.read.contains_key(&key) {
            let content = self.gh.content(path, at)?;
            self.read.insert(key.clone(), content);
        }
        Ok(&self.read[&key])
    }

    /// A path's language at a commit: by its name, or with no extension, by
    /// its shebang there. No answer from GitHub is an error, never "not a
    /// script".
    fn language(&mut self, path: &str, at: &str) -> Result<Option<Language>, String> {
        match rules::by_name(path) {
            Some(language) => Ok(language),
            None => Ok(rules::by_shebang(self.get(path, at)?)),
        }
    }
}

fn text<'a>(file: &'a Value, key: &str) -> &'a str {
    file[key].as_str().unwrap_or_default()
}

/// Read the diff of `head` over `base`, where `lines` is how long a code
/// file may grow.
pub fn read(gh: &dyn GitHub, base: &str, head: &str, lines: usize) -> Result<Diff, String> {
    let compare = gh.compare(base, head)?;
    // The diff's old side: the merge base, never the base branch's tip,
    // which may have moved on.
    let origin = compare["merge_base_commit"]["sha"]
        .as_str()
        .ok_or("the comparison has no merge base")?;
    let files = compare["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut contents = Contents {
        gh,
        read: HashMap::new(),
    };
    let mut diff = Diff {
        listed: files.len(),
        ..Diff::default()
    };
    for f in files {
        let (status, file) = (text(f, "status"), text(f, "filename"));
        let previous = f["previous_filename"].as_str().unwrap_or(file);
        let added = f["additions"].as_i64().unwrap_or_default();
        let removed = f["deletions"].as_i64().unwrap_or_default();
        let was = match status {
            "added" | "copied" => None,
            _ => contents.language(previous, origin)?,
        };
        let now = match status {
            "removed" => None,
            _ => contents.language(file, head)?,
        };
        match (was, now) {
            (Some(Language::Python), Some(_)) | (Some(Language::Shell), Some(Language::Shell)) => {
                diff.growth += added - removed;
            }
            (_, Some(Language::Python)) => diff.new_python.push(file.to_owned()),
            (None, Some(Language::Shell)) => diff.growth += added,
            (Some(_), None) => diff.growth -= removed,
            (None, None) => {}
        }
        if status != "removed" && added > removed && rules::is_code(file, now) {
            let length = contents
                .get(file, head)?
                .iter()
                .filter(|b| **b == b'\n')
                .count();
            if length > lines {
                diff.big.push((file.to_owned(), length));
            }
        }
    }
    diff.timers = adding(files, false, |l| rules::timer(l) && !rules::comment(l));
    diff.models = adding(files, true, rules::model);
    Ok(diff)
}

/// The files with a line the PR adds that `matches`, read from each file's
/// patch, Markdown's too or not. GitHub gives no patch for a binary file or
/// one whose diff is too big to show, so those aren't read.
fn adding(files: &[Value], markdown: bool, matches: impl Fn(&str) -> bool) -> Vec<String> {
    files
        .iter()
        .filter(|f| {
            let name = text(f, "filename").to_lowercase();
            markdown || !(name.ends_with(".md") || name.ends_with(".markdown"))
        })
        .filter(|f| {
            f["patch"].as_str().is_some_and(|patch| {
                patch
                    .lines()
                    .filter_map(|l| l.strip_prefix('+'))
                    .any(&matches)
            })
        })
        .map(|f| text(f, "filename").to_owned())
        .collect()
}
