//! A PR's commits, as the steward judges its head by them
//! (modules/autoland/touches.py): every commit on the head since it left its
//! base, read whole from GitHub's compare of the two, a page at a time.
//! GitHub's list of a PR's commits, and the compare unpaged, stop at 250;
//! paged, the compare lists them all. From them: whether a Codex job wrote
//! any (an `Engine: codex` trailer, which sends the PR to a Claude judge),
//! and whether anyone but the bots it's given authored, co-authored or
//! committed one (a dependency update someone edited).

use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use burrow_github::{call, encode};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::pass::Pass;

/// The trailer burrow-proj's commit-msg hook marks a Codex job's commits
/// with, compared case-blind with each trailer line whole.
const MARK: &str = "engine: codex";

/// A co-author line, and the address it names.
static CO_AUTHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^co-authored-by:.*<([^<>]*)>\s*$").expect("a regex"));

/// What the steward asks `commits`, on stdin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// The PR's head, as its sha, so what's read is that head's whatever
    /// the PR does meanwhile; and its base branch's name.
    pub head: String,
    pub base: String,
    /// The bots' addresses: an author or co-author who's none of these
    /// edited the branch.
    pub bots: Vec<String>,
    /// And a committer who's none of these.
    pub committers: Vec<String>,
}

impl Ask {
    pub fn wrong(&self) -> Option<String> {
        if !(self.head.len() == 40 && self.head.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Some(format!("{:?} isn't a commit's sha", self.head));
        }
        self.base
            .is_empty()
            .then(|| "the base branch has no name".into())
    }
}

/// What the read says of the head.
#[derive(Debug, PartialEq)]
pub enum Read {
    /// Every commit read: whether a Codex job wrote one, and whether
    /// anyone but the bots made one.
    Whole { codex: bool, edited: bool },
    /// GitHub lists fewer commits than its own count: with the head and
    /// the merge base fixed the range can't change, so that's its cap, and
    /// no later read says more. Why, for the PR's status.
    Short(String),
}

/// The head's commits, read whole, or why they couldn't be this time (a
/// page GitHub didn't give, pages that disagree because the base moved
/// between them): no evidence about the head, so it's asked again.
pub fn read(p: &mut Pass, ask: &Ask) -> Result<Read, String> {
    let mut pages = vec![];
    let mut next = Some(format!(
        "repos/{}/compare/{}...{}?per_page=100",
        p.repo,
        encode(&ask.base),
        encode(&ask.head)
    ));
    while let Some(at) = next {
        let r = call(p.api, "GET", &at, None, None).map_err(|e| p.github(e))?;
        pages.push(r.json().map_err(|e| p.github(e))?);
        next = r.next(&at);
    }
    match whole(&pages, &ask.head)? {
        Whole::Short(why) => Ok(Read::Short(why)),
        Whole::Commits(commits) => judge(&commits, ask),
    }
}

enum Whole<'a> {
    Commits(Vec<&'a Value>),
    Short(String),
}

/// The commits the pages hold, if they hold together: each page of the same
/// compare (one count, one merge base), each commit with a sha and a
/// message, none twice, the head among them.
fn whole<'a>(pages: &'a [Value], head: &str) -> Result<Whole<'a>, String> {
    let (mut total, mut fork) = (None, None);
    let mut commits = vec![];
    for page in pages {
        let (Some(t), Some(f), Some(list)) = (
            page["total_commits"].as_u64(),
            page["merge_base_commit"]["sha"].as_str(),
            page["commits"].as_array(),
        ) else {
            return Err("a page of the compare isn't one".into());
        };
        if *total.get_or_insert(t) != t || *fork.get_or_insert(f) != f {
            return Err("the compare's pages disagree: its base moved during the read".into());
        }
        commits.extend(list);
    }
    let total = total.ok_or("the compare has no pages")?;
    let mut shas = HashSet::new();
    for c in &commits {
        let (Some(sha), Some(_)) = (c["sha"].as_str(), c["commit"]["message"].as_str()) else {
            return Err("a commit with no sha or message".into());
        };
        if !shas.insert(sha) {
            return Err(format!("commit {sha} is listed twice"));
        }
    }
    let listed = commits.len() as u64;
    if listed > total {
        return Err(format!("{listed} commits listed of {total}"));
    }
    if listed < total {
        return Ok(Whole::Short(format!(
            "GitHub lists {listed} of its {total} commits, so who wrote the rest can't be read"
        )));
    }
    if !shas.contains(head) {
        return Err(format!("the compare doesn't list the head {head}"));
    }
    Ok(Whole::Commits(commits))
}

/// Whether a Codex job wrote any of them, and whether anyone but the bots
/// made one. A missing or empty address is no one's in particular, so
/// never a bot's.
fn judge(commits: &[&Value], ask: &Ask) -> Result<Read, String> {
    let of = |list: &[String], e: Option<&str>| {
        e.is_some_and(|e| !e.is_empty() && list.iter().any(|b| b == e))
    };
    let (mut codex, mut edited) = (false, false);
    for c in commits {
        let c = &c["commit"];
        let lines = trailers(c["message"].as_str().unwrap_or_default())?;
        codex |= lines.iter().any(|l| l.to_lowercase() == MARK);
        edited |= !of(&ask.bots, c["author"]["email"].as_str())
            || !of(&ask.committers, c["committer"]["email"].as_str())
            || co_authors(&lines).iter().any(|a| !of(&ask.bots, *a));
    }
    Ok(Read::Whole { codex, edited })
}

/// The addresses a commit's Co-authored-by trailers name, as GitHub reads
/// them; None for one that names none.
fn co_authors(trailers: &[String]) -> Vec<Option<&str>> {
    trailers
        .iter()
        .filter(|t| t.to_lowercase().starts_with("co-authored-by:"))
        .map(|t| {
            CO_AUTHOR
                .captures(t)
                .and_then(|m| m.get(1))
                .map(|m| m.as_str())
        })
        .collect()
}

/// A message's trailers, a line each, as `git interpret-trailers --parse`
/// reads them with git's defaults alone, outside any repository, as
/// burrow-proj's commit-msg hook reads the mark it writes.
fn trailers(message: &str) -> Result<Vec<String>, String> {
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git interpret-trailers: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(message.as_bytes())
        .map_err(|e| format!("git interpret-trailers: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git interpret-trailers: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git interpret-trailers: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(String::from)
        .collect())
}

#[cfg(test)]
#[path = "commits_tests.rs"]
mod tests;
