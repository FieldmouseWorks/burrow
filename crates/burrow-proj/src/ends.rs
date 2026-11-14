//! What ends a job's wait on a PR ([`Ends`]), and whether it has ([`ended`]).
//!
//! `proj await URL` ends on any change to the PR or issue (its
//! [`crate::github::fingerprint`]). `proj await --ci URL` ends only on its
//! head's CI verdict (every check green, or any red, even with others still
//! running) and `--review` only on new review findings or threads; both
//! also end when the PR is merged or closed, and both given end on either.
//! A clean review, a comment, a base merge (a new head whose CI hasn't
//! decided), and checks finishing, cancelled or skipped without a verdict
//! end neither.
//!
//! The condition is kept in the await's baseline (its `ends` key), so the
//! watcher reads it from the same line the wait was registered with, across
//! restarts; a baseline without one, as every proj before this wrote, ends
//! on any change as it always did. The fingerprint's derived facts (`ci`,
//! `findings`) are what the conditions read; an any-change wait ignores
//! them, so a baseline from before them isn't news by lacking them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::github::{changes, over};

/// One condition a wait can declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum End {
    /// The head's CI has a verdict: all green, or any red.
    Ci,
    /// New review findings or threads.
    Review,
}

/// What ends a wait: any change when empty, else any one of these (or the
/// PR merging or closing).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ends(pub BTreeSet<End>);

/// The fingerprint keys derived for the conditions, and the baseline's own:
/// none of them is a change an any-change wait hears about.
const DERIVED: [&str; 3] = ["ci", "findings", "ends"];

impl Ends {
    /// `--ci` and `--review` from the front of `args`, and the rest.
    pub fn parse(args: &[String]) -> (Ends, &[String]) {
        let mut ends = BTreeSet::new();
        let mut rest = args;
        while let Some((first, more)) = rest.split_first() {
            match first.as_str() {
                "--ci" => ends.insert(End::Ci),
                "--review" => ends.insert(End::Review),
                _ => break,
            };
            rest = more;
        }
        (Ends(ends), rest)
    }

    pub fn any_change(&self) -> bool {
        self.0.is_empty()
    }

    /// The condition a baseline was registered with.
    pub fn of(was: &Map<String, Value>) -> Ends {
        was.get("ends")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// The baseline to keep for a wait on this condition, from the
    /// fingerprint taken now: the condition goes in it, and an any-change
    /// wait keeps none of the derived facts, so the baseline reads as any
    /// proj before this wrote one. Err when the target can't have it: CI
    /// and reviews are a PR's.
    pub fn baseline(&self, fp: &Value) -> Result<Value, &'static str> {
        let Value::Object(fp) = fp else {
            return Ok(fp.clone());
        };
        let mut kept = fp.clone();
        if self.any_change() {
            for k in DERIVED {
                kept.remove(k);
            }
            return Ok(Value::Object(kept));
        }
        if !fp.contains_key("ci") {
            return Err("--ci and --review wait on a PR, not an issue");
        }
        kept.insert("ends".into(), json!(self));
        Ok(Value::Object(kept))
    }

    /// Words for the job, as in "resumed once TARGET ...".
    pub fn says(&self) -> &'static str {
        match (self.0.contains(&End::Ci), self.0.contains(&End::Review)) {
            (false, false) => "changes",
            (true, false) => "has a CI verdict (all green or any red), or is merged or closed",
            (false, true) => "gets review findings or threads, or is merged or closed",
            (true, true) => {
                "has a CI verdict (all green or any red) or gets review findings or threads, or is merged or closed"
            }
        }
    }
}

/// What a wait registered at `was` hears now that its target is `now`, or
/// None when what it waits for hasn't happened: the news, a line's worth.
pub fn ended(was: &Map<String, Value>, now: &Map<String, Value>) -> Option<String> {
    let ends = Ends::of(was);
    if ends.any_change() {
        let strip = |m: &Map<String, Value>| {
            let mut m = m.clone();
            for k in DERIVED {
                m.remove(k);
            }
            m
        };
        let (was, now) = (strip(was), strip(now));
        return (was != now).then(|| changes(&was, &now));
    }
    let wrap = |m: &Map<String, Value>| Value::Object(m.clone());
    let (old, new) = (wrap(was), wrap(now));
    if over(&new) != over(&old) {
        return Some(over(&new).unwrap_or("reopened").to_owned());
    }
    let mut news = Vec::new();
    let ci = |m: &Map<String, Value>| m.get("ci").cloned().unwrap_or(Value::Null);
    let (ci_was, ci_now) = (ci(was), ci(now));
    if ends.0.contains(&End::Ci)
        && let Some(v) = verdict(&ci_now)
        && verdict(&ci_was) != Some(v)
    {
        news.push(v.say(&ci_now));
    }
    if ends.0.contains(&End::Review) {
        let count = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
        let threads = count(now, "review_comments").saturating_sub(count(was, "review_comments"));
        let findings = count(now, "findings").saturating_sub(count(was, "findings"));
        if findings > 0 {
            news.push(format!("{findings} new review(s) with findings"));
        }
        if threads > 0 {
            news.push(format!("{threads} new review comment(s) in threads"));
        }
    }
    (!news.is_empty()).then(|| news.join("; "))
}

/// Whether the PR's head still has CI to come: some check queued or
/// running. A CI wait isn't over for its deadline then, since an ordinary
/// queue is no reason to wake the job ([`crate::watch`]).
pub fn ci_running(fp: &Value) -> bool {
    over(fp).is_none() && fp["ci"]["verdict"] == "running"
}

/// A decided CI verdict, the head it's on and its failures: what a CI wait
/// compares. None while it's undecided.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict<'a> {
    Green(&'a Value),
    Red(&'a Value, &'a Value),
}

impl Verdict<'_> {
    fn say(&self, ci: &Value) -> String {
        let head: String = ci["head"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(7)
            .collect();
        match self {
            Verdict::Green(_) => format!("CI on {head} is all green"),
            Verdict::Red(_, failed) => {
                let names: Vec<&str> = failed
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let still = if ci["running"].as_bool() == Some(true) {
                    " (other checks still running)"
                } else {
                    ""
                };
                format!("CI on {head} failed: {}{still}", names.join(", "))
            }
        }
    }
}

fn verdict(ci: &Value) -> Option<Verdict<'_>> {
    match ci["verdict"].as_str()? {
        "green" => Some(Verdict::Green(&ci["head"])),
        "red" => Some(Verdict::Red(&ci["head"], &ci["failed"])),
        _ => None,
    }
}

/// The head's CI, from its check runs and combined status as REST gives
/// them: `red` once any check failed (whatever else still runs), else
/// `running` while any is queued or in progress, `green` once every one
/// passed (success, neutral or skipped), and `none` when there are none or
/// they finished without a verdict (cancelled, stale).
pub fn ci(head: &str, runs: &[Value], status: &Value) -> Value {
    const RED: [&str; 4] = ["failure", "timed_out", "action_required", "startup_failure"];
    const GREEN: [&str; 3] = ["success", "neutral", "skipped"];
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let mut failed: Vec<String> = runs
        .iter()
        .filter(|r| {
            r["status"] == "completed"
                && RED.contains(&r["conclusion"].as_str().unwrap_or_default())
        })
        .map(|r| text(&r["name"]))
        .collect();
    let statuses: Vec<&Value> = status["statuses"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let has_statuses = status["total_count"]
        .as_u64()
        .unwrap_or(statuses.len() as u64)
        > 0;
    failed.extend(
        statuses
            .iter()
            .filter(|s| matches!(s["state"].as_str(), Some("failure" | "error")))
            .map(|s| format!("status {}", text(&s["context"]))),
    );
    if failed.is_empty()
        && has_statuses
        && matches!(status["state"].as_str(), Some("failure" | "error"))
    {
        failed.push("status".into());
    }
    failed.sort();
    failed.dedup();
    let running = runs.iter().any(|r| r["status"] != "completed")
        || has_statuses && status["state"] == "pending";
    let verdict = if !failed.is_empty() {
        "red"
    } else if running {
        "running"
    } else if (!runs.is_empty() || has_statuses)
        && runs
            .iter()
            .all(|r| GREEN.contains(&r["conclusion"].as_str().unwrap_or_default()))
        && (!has_statuses || status["state"] == "success")
    {
        "green"
    } else {
        "none"
    };
    json!({"head": head, "verdict": verdict, "failed": failed, "running": running})
}

/// How many of a PR's reviews have findings: changes requested, a review
/// the steward posted whose mark counts findings (`<!-- NAME-review:v1
/// {"findings": N, ...} -->`), or a comment review with something to say
/// and no such mark. A clean one (approved, or marked with none) isn't.
pub fn findings(reviews: &[Value]) -> usize {
    reviews.iter().filter(|r| has_findings(r)).count()
}

fn has_findings(review: &Value) -> bool {
    if review["state"] == "CHANGES_REQUESTED" {
        return true;
    }
    let body = review["body"].as_str().unwrap_or_default();
    let marks: Vec<u64> = body
        .match_indices("-review:v1 ")
        .filter_map(|(at, m)| {
            let rest = &body[at + m.len()..];
            let end = rest.find(" -->")?;
            serde_json::from_str::<Value>(&rest[..end]).ok()
        })
        .map(|m| m["findings"].as_u64().unwrap_or(0))
        .collect();
    if !marks.is_empty() {
        return marks.iter().any(|&k| k > 0);
    }
    review["state"] == "COMMENTED" && !body.trim().is_empty()
}

#[cfg(test)]
#[path = "ends_tests.rs"]
mod tests;
