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
    /// either, files that stop being either included. A file that becomes
    /// shell counts its whole length here, so glue passes and a feature in
    /// bash doesn't, however it got there.
    pub growth: i64,
    /// Code files the PR leaves longer than it found them and past the limit
    /// at its head, with their length there.
    pub big: Vec<(String, usize)>,
    /// Files with a line the PR adds that sets up a timer, outside Markdown
    /// and comments. A file GitHub gives no patch for is read whole.
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

    /// How many lines a file has at a commit, a last one with no newline
    /// included.
    fn length(&mut self, path: &str, at: &str) -> Result<usize, String> {
        let content = self.get(path, at)?;
        let ends = content.last().is_some_and(|b| *b != b'\n');
        Ok(content.iter().filter(|b| **b == b'\n').count() + usize::from(ends))
    }

    /// A file's text at a commit, or None for a binary one: one with a NUL
    /// byte, as git tells them apart.
    fn text(&mut self, path: &str, at: &str) -> Result<Option<String>, String> {
        let content = self.get(path, at)?;
        Ok((!content.contains(&0)).then(|| String::from_utf8_lossy(content).into_owned()))
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
            (None, Some(Language::Shell)) => {
                diff.growth += i64::try_from(contents.length(file, head)?).unwrap_or(i64::MAX);
            }
            (Some(_), None) => diff.growth -= removed,
            (None, None) => {}
        }
        if status != "removed" && added > removed && rules::is_code(file, now) {
            let length = contents.length(file, head)?;
            if length > lines {
                diff.big.push((file.to_owned(), length));
            }
        }
        let new = adding(f, &mut contents, (previous, origin), head)?;
        let lower = file.to_lowercase();
        let markdown = lower.ends_with(".md") || lower.ends_with(".markdown");
        if !markdown && new.iter().any(|l| rules::timer(l) && !rules::comment(l)) {
            diff.timers.push(file.to_owned());
        }
        if new.iter().any(|l| rules::model(l)) {
            diff.models.push(file.to_owned());
        }
    }
    Ok(diff)
}

/// The lines a file's diff adds, from its patch. GitHub gives none for a
/// binary file or one whose diff is too big to show, or one with fewer
/// added lines than it counts, so then they're the lines its head has more
/// of than its old side (`was`, a path at the merge base), and a binary file
/// adds none.
fn adding(
    f: &Value,
    contents: &mut Contents,
    was: (&str, &str),
    head: &str,
) -> Result<Vec<String>, String> {
    let additions = f["additions"].as_i64().unwrap_or_default();
    if let Some(patch) = f["patch"].as_str() {
        let added: Vec<String> = patch
            .lines()
            .filter_map(|l| l.strip_prefix('+'))
            .map(str::to_owned)
            .collect();
        if i64::try_from(added.len()).unwrap_or(i64::MAX) >= additions {
            return Ok(added);
        }
    }
    let (status, file) = (text(f, "status"), text(f, "filename"));
    if status == "removed" || additions == 0 {
        return Ok(vec![]);
    }
    let Some(now) = contents.text(file, head)? else {
        return Ok(vec![]);
    };
    let mut had: HashMap<String, usize> = HashMap::new();
    if status != "added" {
        for line in contents.text(was.0, was.1)?.unwrap_or_default().lines() {
            *had.entry(line.to_owned()).or_default() += 1;
        }
    }
    let mut added = vec![];
    for line in now.lines() {
        match had.get_mut(line) {
            Some(n) if *n > 0 => *n -= 1,
            _ => added.push(line.to_owned()),
        }
    }
    Ok(added)
}
