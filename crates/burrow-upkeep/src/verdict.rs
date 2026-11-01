//! What each of a PR's reviewers last said of its head, read back from the
//! reviews the steward posted for them (post.rs). Each such review carries
//! a mark of what it was a look at, `<!-- REVIEWER-review:v1 {...} -->`:
//! the head and base the reviewer read, how many findings it had, and the
//! description it was shown. The mark is written here (`mark`) and read
//! here (`said`), and only the steward's own count, since anyone can write
//! one. The pass hands over the PR's reviews as it read them
//! (review_step.py) and gets each reviewer's verdict back.

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::flaky::login;
use crate::post::number;

/// What the pass hands `verdict`, on stdin: the PR as it is now.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asked {
    pub head: String,
    pub base: String,
    /// The fingerprint of its description (common.py `fingerprint`).
    pub description: String,
    /// The reviewers to say of, as their marks name them.
    pub reviewers: Vec<String>,
    /// Its reviews, as GraphQL gives them (prs.py).
    pub reviews: Vec<Review>,
}

/// One of a PR's reviews, as GraphQL gives it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    /// Who posted it (null for a deleted account).
    #[serde(default)]
    pub author: Value,
    pub body: Option<String>,
    /// The commit it's on.
    pub commit: Option<Commit>,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Commit {
    pub oid: String,
}

/// What a reviewer last said of the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Nothing that covers the PR as it is now.
    None,
    Clean,
    Findings,
}

/// Each reviewer's verdict on the PR, for its step.
#[derive(Debug, PartialEq, Serialize)]
pub struct Said {
    /// What the reviewer last said of this head on this base, shown this
    /// description.
    pub verdict: Verdict,
    /// When it said it: its review's time, in seconds (0 with none).
    pub at: f64,
    /// Its reviews of the PR that had findings, at any head, on any base:
    /// each one is a round.
    pub rounds: u64,
}

impl Asked {
    /// Why it can't be read as given: a reviewer whose name isn't one.
    pub fn wrong(&self) -> Option<String> {
        self.reviewers
            .iter()
            .find(|r| !named(r))
            .map(|r| format!("{r:?} isn't a reviewer's name"))
    }
}

/// Whether `r` is a reviewer's name: lowercase letters, which name its
/// queue entries, its sessions and its marks.
pub fn named(r: &str) -> bool {
    !r.is_empty() && r.bytes().all(|b| b.is_ascii_lowercase())
}

/// The mark on the reviewer's review of `head` on `base`: how many
/// findings it had, and the fingerprint of the description it was shown,
/// or None when that isn't known (the review then covers no description).
pub fn mark(
    reviewer: &str,
    head: &str,
    base: &str,
    findings: usize,
    description: Option<&str>,
) -> String {
    let said =
        json!({"commit_sha": head, "base": base, "findings": findings, "description": description});
    format!("<!-- {reviewer}-review:v1 {said} -->")
}

/// Whether a review shown the description `shown` (its fingerprint, None
/// when its mark doesn't say) covers the PR's now, `now`. The reasons a
/// description gives are part of what a reviewer judges, so a review of
/// another one doesn't cover the PR: a `Why not Rust:` line written in
/// after a clean review (CI wanted one) has had nobody weigh it.
pub fn covers(shown: Option<&str>, now: &str) -> bool {
    shown == Some(now)
}

/// What each reviewer last said of the PR as it is now, from the reviews
/// `me` (the steward, as REST gives its login) posted: the newest of them
/// marked for this head on this base, shown this description, and on this
/// head. Retargeted, the PR is another change on the same head, so what
/// was said on its old base doesn't count. Fails on a review whose time
/// isn't one.
pub fn said(me: &str, asked: &Asked) -> Result<BTreeMap<String, Said>, String> {
    asked
        .reviewers
        .iter()
        .map(|r| Ok((r.clone(), one(me, asked, r)?)))
        .collect()
}

fn one(me: &str, asked: &Asked, reviewer: &str) -> Result<Said, String> {
    let pat = Regex::new(&format!(
        r"<!-- {}-review:v1 (\{{.*?\}}) -->",
        regex::escape(reviewer)
    ))
    .map_err(|e| e.to_string())?;
    let mut s = Said {
        verdict: Verdict::None,
        at: 0.0,
        rounds: 0,
    };
    for x in &asked.reviews {
        if login(&x.author) != me {
            continue;
        }
        let Some(m) = x
            .body
            .as_deref()
            .and_then(|b| pat.captures(b))
            .and_then(|c| serde_json::from_str::<Map<String, Value>>(&c[1]).ok())
        else {
            continue;
        };
        let found = match m.get("findings") {
            None => 0,
            Some(k) => match number(k) {
                Some(k) => k,
                None => continue,
            },
        } > 0;
        s.rounds += u64::from(found);
        let text = |k: &str| m.get(k).and_then(Value::as_str);
        if text("commit_sha") == Some(&asked.head)
            && text("base") == Some(&asked.base)
            && covers(text("description"), &asked.description)
            && x.commit.as_ref().map(|c| c.oid.as_str()) == Some(&asked.head)
        {
            let at = match x.submitted_at.as_deref() {
                None | Some("") => 0.0,
                Some(t) => crate::ts(t)?,
            };
            if at >= s.at {
                s.verdict = if found {
                    Verdict::Findings
                } else {
                    Verdict::Clean
                };
                s.at = at;
            }
        }
    }
    Ok(s)
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
