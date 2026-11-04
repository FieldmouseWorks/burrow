//! One Renovate PR as the sweep reads it, from what `gh pr list --json`
//! says of it: its checks as the steward has them, whether it's a major or
//! a security fix, and the verdicts our login sealed on it. Its fields read
//! as the script's `jq -r` read them.

use std::sync::LazyLock;

use md5::{Digest, Md5};
use regex::Regex;
use serde_json::Value;

use super::shown;

/// A major as the steward takes one (dep_kind in modules/autoland/touches.py)
/// by its title: from one major version to another.
static FROM_TO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"from v?(?<a>[0-9]+)\S* to v?(?<b>[0-9]+)").expect("a regex"));

/// A job's verdict on a major, as major-seal prints it.
static VERDICT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"<!-- major-verdict: (?<v>ok|held) (?<sha>[0-9a-f]{40}) on (?<base>\S+) (?<mac>[0-9a-f]{32}) -->",
    )
    .expect("a regex")
});

/// A check's state, as the steward reads it (ci_state in
/// modules/autoland/prs.py), so a head neither calls the other's is never
/// left between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Green,
    Pending,
    Red,
}

/// A check on a PR's head, but the steward's own status.
#[derive(Debug)]
pub(crate) struct Check {
    pub name: Value,
    /// A proof the steward runs on a machine: no CI to wait for.
    pub proof: bool,
    /// When it started (a status) or finished (a check run).
    pub at: Value,
    pub state: State,
}

pub(crate) struct Pr<'a>(pub &'a Value);

impl Pr<'_> {
    /// `jq -r .KEY`, as `$(...)` has it.
    pub fn text(&self, key: &str) -> String {
        shown(self.0.get(key)).trim_end_matches('\n').to_string()
    }

    /// Whether it has the label `name`.
    pub fn labelled(&self, name: &str) -> bool {
        let labels = items(self.0.get("labels"));
        labels
            .iter()
            .any(|l| l.get("name").and_then(Value::as_str) == Some(name))
    }

    /// Renovate's own security fix: it merges itself, a major too.
    pub fn security(&self) -> bool {
        self.labelled("security")
    }

    /// A major, as the steward takes one: the label, or a title from one
    /// major version to another. A security fix never is.
    pub fn major(&self) -> bool {
        if self.security() {
            return false;
        }
        let bumped = self
            .0
            .get("title")
            .and_then(Value::as_str)
            .and_then(|t| FROM_TO.captures(t))
            .is_some_and(|m| m["a"] != m["b"]);
        self.labelled("major") || bumped
    }

    /// When Renovate opened it, in seconds since the epoch.
    pub fn created(&self) -> Option<i64> {
        let at: jiff::Timestamp = self.0.get("createdAt")?.as_str()?.parse().ok()?;
        Some(at.as_second())
    }

    /// Each check on its head but the steward's own status (`settled`),
    /// with whether it's one of the steward's proofs (`proofs`, their
    /// names).
    pub fn checks(&self, settled: &str, proofs: &[Value]) -> Vec<Check> {
        items(self.0.get("statusCheckRollup"))
            .into_iter()
            .filter_map(|c| {
                let name = alt(c.get("context"), c.get("name"));
                if name.as_str() == Some(settled) {
                    return None;
                }
                let status = c.get("__typename").and_then(Value::as_str) == Some("StatusContext");
                let field = |k: &str| c.get(k).and_then(Value::as_str);
                let state = if status {
                    match field("state") {
                        Some("SUCCESS") => State::Green,
                        Some("PENDING" | "EXPECTED") => State::Pending,
                        _ => State::Red,
                    }
                } else if field("status") != Some("COMPLETED") {
                    State::Pending
                } else if matches!(field("conclusion"), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) {
                    State::Green
                } else {
                    State::Red
                };
                let at = c
                    .get(if status { "startedAt" } else { "completedAt" })
                    .cloned()
                    .unwrap_or(Value::Null);
                Some(Check {
                    proof: proofs.contains(&name),
                    name,
                    at,
                    state,
                })
            })
            .collect()
    }
}

/// Which CI on a head finished, and when, as one hash: it changes when CI
/// on that head runs again (a job's rerun) or turns up late (after an empty
/// rollup). The steward's proofs aren't CI.
pub(crate) fn finished(checks: &[Check]) -> String {
    let mut lines: Vec<String> = checks
        .iter()
        .filter(|c| c.state != State::Pending && !c.proof)
        .map(|c| format!("{} {}", shown(Some(&c.name)), shown(Some(&c.at))))
        .collect();
    lines.sort();
    let digest = Md5::digest(format!("{}\n", lines.join("\n")));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The verdicts `me` marked on `sha` for `base` in what `gh pr view --json
/// comments` said, oldest first: each (ok|held, its seal), to check with
/// major-seal.
pub(crate) fn verdicts(said: &[u8], me: &str, sha: &str, base: &str) -> Vec<(String, String)> {
    let Ok(said) = serde_json::from_slice::<Value>(said) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let Some(Value::Array(comments)) = said.get("comments") else {
        return found;
    };
    for c in comments {
        let login = match c.get("author") {
            None | Some(Value::Null) => None,
            Some(Value::Object(a)) => a.get("login"),
            // jq stops at what it can't read.
            Some(_) => break,
        };
        if login.and_then(Value::as_str) != Some(me) {
            continue;
        }
        let Some(body) = c.get("body").and_then(Value::as_str) else {
            break;
        };
        for m in VERDICT.captures_iter(body) {
            if &m["sha"] == sha && &m["base"] == base {
                found.push((m["v"].to_string(), m["mac"].to_string()));
            }
        }
    }
    found
}

/// The elements of an array (jq's `.[]?`): an object's values too.
fn items(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    }
}

/// jq's `A // B`: A, unless it's missing, null or false.
fn alt(a: Option<&Value>, b: Option<&Value>) -> Value {
    match a {
        Some(Value::Null | Value::Bool(false)) | None => b.cloned().unwrap_or(Value::Null),
        Some(v) => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr(labels: &[&str], title: &str) -> Value {
        json!({"title": title, "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>()})
    }

    #[test]
    fn a_major_is_the_label_or_a_title_from_one_major_to_another_but_never_a_security_fix() {
        assert!(Pr(&pr(&["major"], "Update foo to v2")).major());
        assert!(Pr(&pr(&[], "Update foo from 1.4 to 2.0")).major());
        assert!(Pr(&pr(&[], "Update foo from v1.4.2 to v2")).major());
        assert!(!Pr(&pr(&[], "Update foo from 1.4 to 1.5")).major());
        // As the steward compares them: as text.
        assert!(Pr(&pr(&[], "Update foo from 01 to 1")).major());
        assert!(!Pr(&pr(&["major", "security"], "Update foo to v2")).major());
        assert!(Pr(&pr(&["major", "security"], "x")).security());
    }

    #[test]
    fn checks_are_green_pending_or_red_as_the_steward_has_them() {
        let p = json!({"statusCheckRollup": [
            {"__typename": "CheckRun", "name": "a", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "t1"},
            {"__typename": "CheckRun", "name": "b", "status": "COMPLETED", "conclusion": "SKIPPED"},
            {"__typename": "CheckRun", "name": "c", "status": "COMPLETED", "conclusion": "NEUTRAL"},
            {"__typename": "CheckRun", "name": "d", "status": "COMPLETED", "conclusion": "CANCELLED"},
            {"__typename": "CheckRun", "name": "e", "status": "IN_PROGRESS", "conclusion": ""},
            {"__typename": "StatusContext", "context": "f", "state": "SUCCESS", "startedAt": "t2"},
            {"__typename": "StatusContext", "context": "g", "state": "EXPECTED"},
            {"__typename": "StatusContext", "context": "h", "state": "PENDING"},
            {"__typename": "StatusContext", "context": "i", "state": "ERROR"},
            {"__typename": "StatusContext", "context": "review-settled", "state": "PENDING"},
        ]});
        let checks = Pr(&p).checks("review-settled", &[json!("h")]);
        let states: Vec<(String, State, bool)> = checks
            .iter()
            .map(|c| (shown(Some(&c.name)), c.state, c.proof))
            .collect();
        use State::*;
        assert_eq!(
            states,
            [
                ("a".into(), Green, false),
                ("b".into(), Green, false),
                ("c".into(), Green, false),
                ("d".into(), Red, false),
                ("e".into(), Pending, false),
                ("f".into(), Green, false),
                ("g".into(), Pending, false),
                ("h".into(), Pending, true),
                ("i".into(), Red, false),
            ]
        );
        assert_eq!(checks[0].at, json!("t1"));
        assert_eq!(checks[5].at, json!("t2"));
        assert_eq!(checks[1].at, Value::Null);
    }

    #[test]
    fn what_finished_is_hashed_as_md5sum_hashed_the_scripts_lines() {
        // printf 'a t1\nb null\n' | md5sum
        let p = json!({"statusCheckRollup": [
            {"__typename": "CheckRun", "name": "b", "status": "COMPLETED", "conclusion": "FAILURE"},
            {"__typename": "CheckRun", "name": "a", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "t1"},
            {"__typename": "CheckRun", "name": "c", "status": "QUEUED"},
        ]});
        assert_eq!(
            finished(&Pr(&p).checks("x", &[])),
            "968e5529f16fa11522c0056e79e31678"
        );
        // Nothing finished: `printf '\n' | md5sum`.
        assert_eq!(finished(&[]), "68b329da9893e34099c7d8ad5cb9c940");
    }

    #[test]
    fn a_verdict_is_ours_for_this_head_on_this_base() {
        let sha = "1".repeat(40);
        let mac = "a".repeat(32);
        let marker =
            |v: &str, base: &str| format!("<!-- major-verdict: {v} {sha} on {base} {mac} -->");
        let said = json!({"comments": [
            {"author": {"login": "me"}, "body": format!("x {} y {}", marker("ok", "main"), marker("held", "main"))},
            {"author": {"login": "mallory"}, "body": marker("held", "main")},
            {"author": {"login": "me"}, "body": marker("held", "release")},
            {"author": null, "body": marker("ok", "main")},
        ]});
        let found = verdicts(said.to_string().as_bytes(), "me", &sha, "main");
        assert_eq!(
            found,
            [("ok".into(), mac.clone()), ("held".into(), mac.clone())]
        );
        assert!(verdicts(b"not json", "me", &sha, "main").is_empty());
    }
}
