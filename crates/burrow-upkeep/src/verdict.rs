//! What Codex last said of a PR's head, read from what its GitHub connector
//! (the ChatGPT Codex Connector App, [`CONNECTOR`]) posts on the PR: the
//! summary comment it keeps up to date, whose `codex-review-state:v1` mark
//! names the commit it reviewed, whether that review is done and whether it
//! found anything; and a review with inline findings for each look that
//! found some, naming the commit it read. Only the connector's own posts
//! count, since anyone can write the marks. The pass hands over the PR's
//! reviews and comments as it read them (review_step.py) and gets the
//! verdict back.
//!
//! The connector reviews each push itself, so there's no look to carry from
//! an earlier head: a head is clean only once a review of that exact commit
//! finished with nothing found. Its word binds only the commit, though: a
//! retarget onto another base is another diff, and an edited description is
//! text a review may not have read, and nothing the connector posts says
//! which base or description it read, or when a review began. So a PR that
//! changed either way after its head was pushed, as GitHub dates both, is
//! `changed`: whatever Codex says of it, its word can't land it, and the
//! steward has the PR judged, whose verdict binds the base and description
//! as well as the head (tasks.py `judged_on`). One changed before the push
//! is what every review of the push read.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::flaky::login;

/// The connector's login, as REST gives it (GraphQL's with `[bot]` added:
/// flaky.rs `login`).
pub const CONNECTOR: &str = "chatgpt-codex-connector[bot]";

/// What the pass hands `verdict`, on stdin: the PR as it is now.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asked {
    pub head: String,
    /// Its reviews, as GraphQL gives them (prs.py).
    pub reviews: Vec<Review>,
    /// Its comments, as GraphQL gives them (prs.py).
    pub comments: Vec<Comment>,
    /// When its description was last edited (GraphQL's `lastEditedAt`).
    #[serde(default)]
    pub edited: Option<String>,
    /// When it was last retargeted (its last `BaseRefChangedEvent`).
    #[serde(default)]
    pub retargeted: Option<String>,
    /// When GitHub got its head, in seconds, as near as it says (prs.py
    /// `pushed`), or null when it can't say.
    #[serde(default)]
    pub pushed: Option<f64>,
}

/// One of a PR's reviews, as GraphQL gives it.
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub oid: String,
}

/// One of a PR's comments, as GraphQL gives it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    #[serde(default)]
    pub author: Value,
    pub body: Option<String>,
    pub updated_at: Option<String>,
}

/// What Codex last said of the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Nothing about this head yet, or nothing that's a verdict or a
    /// review under way (a summary that ended with no word on findings).
    None,
    /// A review of this head is under way.
    Running,
    Clean,
    Findings,
}

/// Codex's verdict on the PR, for its step.
#[derive(Debug, PartialEq, Serialize)]
pub struct Said {
    /// What it last said of this head.
    pub verdict: Verdict,
    /// When it said it, in seconds (0 with nothing).
    pub at: f64,
    /// Its reviews of the PR that had findings, at any head: each one is a
    /// round.
    pub rounds: u64,
    /// Whether the PR was retargeted or its description edited after its
    /// head was pushed (or at some time when GitHub can't say when it was
    /// pushed): Codex's word binds no base or description, so it can't
    /// settle the PR alone.
    pub changed: bool,
}

/// The summary's statuses for a review under way: only these are
/// `Running`.
const IN_FLIGHT: [&str; 1] = ["running"];

/// The summary comment's state for one kind of review.
#[derive(Debug, Deserialize)]
struct State {
    commit_sha: Option<String>,
    status: Option<String>,
    has_any_findings: Option<bool>,
}

/// What Codex last said of the PR as it is now. Fails on a time that isn't
/// one.
pub fn said(asked: &Asked) -> Result<Said, String> {
    let state = Regex::new(r"(?s)<!-- codex-review-state:v1 (\{.*?\}) -->").expect("pattern");
    let receipt = Regex::new(r"Reviewed commit:\*\*\s*`([0-9a-f]{7,40})`").expect("pattern");
    let when = |t: &Option<String>| match t.as_deref() {
        None | Some("") => Ok(0.0),
        Some(t) => crate::ts(t),
    };
    let since = when(&asked.edited)?.max(when(&asked.retargeted)?);
    let mut s = Said {
        verdict: Verdict::None,
        at: 0.0,
        rounds: 0,
        changed: since > 0.0 && asked.pushed.is_none_or(|p| since >= p),
    };
    let mut saw = |verdict, at: f64| {
        if at >= s.at {
            s.verdict = verdict;
            s.at = at;
        }
    };
    for c in &asked.comments {
        if login(&c.author) != CONNECTOR {
            continue;
        }
        let Some(m) = c.body.as_deref().and_then(|b| state.captures(b)) else {
            continue;
        };
        let Ok(all) = serde_json::from_str::<Value>(&m[1]) else {
            continue;
        };
        let Ok(code) = serde_json::from_value::<State>(all["code_review"].clone()) else {
            continue;
        };
        if code.commit_sha.as_deref() != Some(asked.head.as_str()) {
            continue;
        }
        let verdict = match (code.status.as_deref(), code.has_any_findings) {
            (Some("completed"), Some(false)) => Verdict::Clean,
            (Some("completed"), Some(true)) => Verdict::Findings,
            (Some(s), _) if IN_FLIGHT.contains(&s) => Verdict::Running,
            // Ended with no word on findings (completed with them null, as
            // on Conary#511), failed, or a state this doesn't know: nothing
            // said of the head, so it's asked once its wait is up (the
            // connector's 👀 still guards an ask it has in hand). Calling
            // it running stalled the PR with nothing under way.
            _ => continue,
        };
        saw(verdict, when(&c.updated_at)?);
    }
    for r in &asked.reviews {
        if login(&r.author) != CONNECTOR {
            continue;
        }
        let Some(read) = r.body.as_deref().and_then(|b| receipt.captures(b)) else {
            continue;
        };
        s.rounds += 1;
        // A receipt names only a prefix of the commit, so it counts only on
        // the commit the review is on: findings there, never clean (the
        // summary says that).
        let on = r.commit.as_ref().map(|c| c.oid.as_str());
        if on == Some(asked.head.as_str()) && asked.head.starts_with(&read[1]) {
            let at = when(&r.submitted_at)?;
            if at >= s.at {
                s.verdict = Verdict::Findings;
                s.at = at;
            }
        }
    }
    Ok(s)
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
