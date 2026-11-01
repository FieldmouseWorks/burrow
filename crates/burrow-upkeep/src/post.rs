//! A reviewer's answer, posted as its review of the head it read: the
//! reviewers' service (crates/burrow-review) leaves the answer, and the
//! repo's next pass hands it here (review_step.py). One review on the head,
//! marked as that reviewer's and with the description it was shown, which
//! the steward reads its verdict back from (reviewers.py `verdict`), and a
//! thread for each finding, on the line it's about.
//!
//! The lines a thread can go on are the new side of GitHub's own compare of
//! the head with the base: its files by name and their hunks, so no name is
//! read out of a diff's text.

use std::collections::BTreeSet;

use burrow_github::{Error, call, encode};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::pass::Pass;

/// What the pass hands `post`, on stdin: the look the reviewers' service
/// answered.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub number: u64,
    /// The head the reviewer read, which its review goes on.
    pub head: String,
    pub base: String,
    /// The reviewer as its marks name it (`codex`), and as it's shown.
    pub reviewer: String,
    pub name: String,
    /// What it answered, as the service left it: anything, until it's read.
    pub answer: Value,
    /// The fingerprint of the description it was shown, or None when that
    /// isn't known: its review then covers no description (`verdict`).
    pub description: Option<String>,
}

/// A review on the head: what its reviewer found, or that it found nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Posted {
    Clean,
    Findings,
}

/// Why there's no review.
#[derive(Debug, PartialEq, Eq)]
pub enum Unposted {
    /// The answer has no list of findings: no review at all, never a clean
    /// one.
    NotAReview(String),
    /// GitHub said no, or gave what won't do: the same answer can go again
    /// next pass.
    Refused(String),
    /// GitHub didn't answer.
    Unanswered(String),
}

/// One finding, as the answer gives it.
struct Finding<'a> {
    path: &'a str,
    title: &'a str,
    body: &'a str,
    /// The line it's about, at the head, if the answer gave a number.
    line: Option<i64>,
    severity: &'a str,
    covers: Option<&'a str>,
}

/// The answer's findings and summary, or why it's no review: anything but
/// a list of findings, each with its path, title and body as text, and a
/// summary that's text if there is one.
fn read(answer: &Value) -> Result<(Vec<Finding<'_>>, &str), String> {
    let wrong = || {
        let said: String = answer.to_string().chars().take(100).collect();
        format!("not a review: {said}")
    };
    let a = answer.as_object().ok_or_else(wrong)?;
    let found = a
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(wrong)?;
    let summary = match a.get("summary") {
        None => "",
        Some(Value::String(s)) => s,
        Some(_) => return Err(wrong()),
    };
    let mut out = vec![];
    for f in found {
        let f = f.as_object().ok_or_else(wrong)?;
        let (Some(path), Some(title), Some(body)) =
            (text(f, "path"), text(f, "title"), text(f, "body"))
        else {
            return Err(wrong());
        };
        out.push(Finding {
            path,
            title,
            body,
            line: f.get("line").and_then(number),
            severity: match text(f, "severity") {
                Some(s @ ("P0" | "P1" | "P2")) => s,
                _ => "P1",
            },
            covers: text(f, "covers").filter(|c| !c.trim().is_empty()),
        });
    }
    Ok((out, summary))
}

/// The text at `k`, if it's text.
fn text<'a>(f: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    f.get(k).and_then(Value::as_str)
}

/// A line number as an answer may give one: a number (a fraction cut off)
/// or a number's text. None for anything else, which goes on the file's
/// first line that can take it.
fn number(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Each file the compare changes, in its order, with the new-side lines a
/// thread can go on: none for a file with no new side (deleted, binary, a
/// rename or mode change alone) or whose patch GitHub doesn't give (too
/// big). None if GitHub's answer has no list of files.
pub fn commentable(compare: &Value) -> Option<Vec<(String, BTreeSet<i64>)>> {
    compare
        .get("files")?
        .as_array()?
        .iter()
        .map(|f| {
            let name = f.get("filename")?.as_str()?;
            let lines = f
                .get("patch")
                .and_then(Value::as_str)
                .map(new_lines)
                .unwrap_or_default();
            Some((name.to_string(), lines))
        })
        .collect()
}

/// The new side's line numbers in a patch's hunks: each line of context or
/// added, counted from its hunk's start. A hunk whose start can't be read
/// counts none.
fn new_lines(patch: &str) -> BTreeSet<i64> {
    let mut lines = BTreeSet::new();
    let mut at = None;
    for x in patch.split('\n') {
        if let Some(head) = x.strip_prefix("@@ ") {
            at = start(head);
        } else if let Some(n) = at.as_mut()
            && (x.starts_with('+') || x.starts_with(' '))
        {
            lines.insert(*n);
            *n += 1;
        }
    }
    lines
}

/// Where a hunk starts on the new side, from its header after `@@ `:
/// `-OLD[,N] +NEW[,N] @@`.
fn start(head: &str) -> Option<i64> {
    let (old, new) = head.split_once(' ')?;
    old.strip_prefix('-')?;
    let digits = new
        .strip_prefix('+')?
        .split(|c: char| !c.is_ascii_digit())
        .next()?;
    digits.parse().ok()
}

/// Model text goes out as the steward's App: no @-mentions from it, and no
/// marks of the steward's.
pub fn quiet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '@'
            && chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_' || *n == '-')
        {
            out.push('\u{200b}');
        }
    }
    out.replace("<!--", "&lt;!--")
}

/// The mark on each finding's thread, which the reviewer's later looks are
/// shown theirs by (queue.rs `prior`).
pub fn finding(reviewer: &str) -> String {
    format!("<!-- {reviewer}-finding -->")
}

/// GitHub's error, as why there's no review.
fn unposted(e: Error) -> Unposted {
    if e.unanswered {
        Unposted::Unanswered(e.why)
    } else {
        Unposted::Refused(e.why)
    }
}

/// The reviewer's answer as one review on the head it read. A finding goes
/// on its line, or the nearest one the PR changes. One on a file with no
/// new side is a thread on the whole file, and one on a file the PR doesn't
/// change (a caller it breaks) is a thread on its first changed file that
/// says where. Both are in the review's own body in full too. With no
/// changed file at all, the finding is listed in the body. Every one counts.
pub fn post(p: &mut Pass, look: &Look) -> Result<Posted, Unposted> {
    let (found, summary) = read(&look.answer).map_err(Unposted::NotAReview)?;
    let (repo, n, head, who) = (p.repo, look.number, &look.head, &look.name);
    let path = format!(
        "repos/{repo}/compare/{}...{}",
        encode(&look.base),
        encode(head)
    );
    let compare = p.get(&path).map_err(unposted)?;
    // Read wrong, every finding would land in the body and the review count
    // as clean.
    let files = commentable(&compare)
        .ok_or_else(|| Unposted::Refused(format!("couldn't read the diff: GET {path}")))?;
    let mark = finding(&look.reviewer);
    let (mut comments, mut whole, mut loose) = (vec![], vec![], vec![]);
    for f in &found {
        let mut msg = format!(
            "**{who} · {}** {}\n\n{}",
            f.severity,
            quiet(f.title),
            quiet(f.body)
        );
        if let Some(c) = f.covers {
            msg.push_str(&format!("\n\n**A complete fix covers:** {}", quiet(c)));
        }
        let listed = format!("- `{}`: {}", quiet(f.path), msg.replace("\n\n", " "));
        match files.iter().find(|(name, _)| name == f.path) {
            None => match files.first() {
                None => loose.push(listed),
                Some((on, _)) => {
                    let body = format!(
                        "{msg}\n\n(In `{}`, which this PR doesn't change.)\n\n{mark}",
                        quiet(f.path)
                    );
                    whole.push((json!({"path": on, "body": body}), listed));
                }
            },
            Some((_, lines)) if lines.is_empty() => {
                let body = format!("{msg}\n\n{mark}");
                whole.push((json!({"path": f.path, "body": body}), listed));
            }
            Some((_, lines)) => {
                let first = *lines.first().unwrap_or(&1);
                let want = f.line.unwrap_or(first);
                let line = if lines.contains(&want) {
                    want
                } else {
                    *lines
                        .iter()
                        .min_by_key(|x| x.abs_diff(want))
                        .unwrap_or(&first)
                };
                let body = format!("{msg}\n\n{mark}");
                comments.push(json!({"path": f.path, "line": line, "side": "RIGHT", "body": body}));
            }
        }
    }
    let k = comments.len() + whole.len() + loose.len();
    let short: String = head.chars().take(7).collect();
    let mut body = format!("**{who} review** of {short}: ");
    body.push_str(&match k {
        0 => "nothing found.".to_string(),
        1 => "1 finding. Fix or answer each, then resolve its thread.".to_string(),
        k => format!("{k} findings. Fix or answer each, then resolve its thread."),
    });
    if !summary.is_empty() {
        body.push_str(&format!("\n\n{}", quiet(summary)));
    }
    if !loose.is_empty() {
        body.push_str("\n\nFindings with no changed file to go on:\n");
        body.push_str(&loose.join("\n"));
    }
    if !whole.is_empty() {
        // In full here too: the threads go on their own, and a finding
        // mustn't hang on one of them.
        body.push_str("\n\nFindings on whole files (each a thread too):\n");
        let listed: Vec<&str> = whole.iter().map(|(_, l)| l.as_str()).collect();
        body.push_str(&listed.join("\n"));
    }
    let said = json!({"commit_sha": head, "base": look.base, "findings": k, "description": look.description});
    body.push_str(&format!("\n\n<!-- {}-review:v1 {said} -->", look.reviewer));
    let review = json!({"commit_id": head, "event": "COMMENT", "body": body, "comments": comments});
    let reviews = format!("repos/{repo}/pulls/{n}/reviews");
    call(p.api, "POST", &reviews, Some(&review), None).map_err(|e| match unposted(p.noted(e)) {
        Unposted::Refused(why) => {
            Unposted::Refused(format!("GitHub didn't take the review: {why}"))
        }
        other => other,
    })?;
    // A review can't hold a thread on a whole file: each goes on its own,
    // after it, so a review GitHub turns away (posted again next pass)
    // leaves none behind. The review's body has them, so one GitHub won't
    // take loses nothing.
    let threads = format!("repos/{repo}/pulls/{n}/comments");
    for (mut c, _) in whole {
        c["commit_id"] = head.as_str().into();
        c["subject_type"] = "file".into();
        if let Err(e) = call(p.api, "POST", &threads, Some(&c), None) {
            let why = p.github(e);
            let on = c["path"].as_str().unwrap_or_default();
            eprintln!("{repo}#{n}: couldn't post {who}'s finding on {on}: {why}");
        }
    }
    eprintln!("{repo}#{n}: {who}'s review posted, {k} finding(s)");
    Ok(if k > 0 {
        Posted::Findings
    } else {
        Posted::Clean
    })
}

#[cfg(test)]
#[path = "post_tests.rs"]
mod tests;
