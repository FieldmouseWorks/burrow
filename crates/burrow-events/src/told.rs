//! What a handler's pass over a repo is told its events were about, when the
//! handler is told them: an issue (`issueDir`, proj-inbox's), or a pull
//! request, a branch or a commit (`prDir`, the steward's). A pass that's
//! told looks at those first, or alone; one that isn't looks at everything.
//!
//! In its file, one a line: an issue as its number (`7`), the rest as
//! `pr 7`, `branch NAME` or `commit SHA`. A kick may carry the same lines
//! (routes.rs), so a pass that couldn't run yet can put its own back.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Told {
    Issue(u64),
    Pr(u64),
    Branch(String),
    Commit(String),
}

impl Told {
    /// Whether this is about a pull request (a PR, a branch, a commit),
    /// rather than an issue.
    pub fn about_pr(&self) -> bool {
        !matches!(self, Told::Issue(_))
    }

    /// One line of a told file or a kick's body; None for anything else.
    pub fn parse(line: &str) -> Option<Told> {
        let line = line.trim();
        if let Ok(n) = line.parse::<u64>() {
            return Some(Told::Issue(n));
        }
        let (kind, rest) = line.split_once(' ')?;
        match kind {
            "pr" => rest.parse().ok().map(Told::Pr),
            "branch" if branch(rest) => Some(Told::Branch(rest.to_owned())),
            "commit" if commit(rest) => Some(Told::Commit(rest.to_owned())),
            _ => None,
        }
    }
}

impl fmt::Display for Told {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Told::Issue(n) => write!(f, "{n}"),
            Told::Pr(n) => write!(f, "pr {n}"),
            Told::Branch(b) => write!(f, "branch {b}"),
            Told::Commit(c) => write!(f, "commit {c}"),
        }
    }
}

/// A branch name as git takes one, as far as a line of the file goes: no
/// whitespace or control characters, and not too long to be one.
fn branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn commit(sha: &str) -> bool {
    (7..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

fn at<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().try_fold(v, |v, k| v.get(k))
}

/// The PRs, branches or commits a delivery is about, for a handler told
/// them; None for one about none of them (the repo itself, an issue, a push
/// to the default branch, which moves every PR's base), whose pass looks at
/// everything.
pub fn prs(event: &str, payload: &Value) -> Option<Vec<Told>> {
    let text = |keys: &[&str]| at(payload, keys).and_then(Value::as_str);
    let number = |keys: &[&str]| at(payload, keys).and_then(Value::as_u64);
    // A check's PRs as GitHub lists them, those of this repo only: one in
    // another repo (a fork's, say, whose head is this branch) has another
    // PR's number here. Without any, its branch, else its commit.
    let checked = |of: &str, head: &[&str]| {
        let repo = text(&["repository", "full_name"]).map(str::to_lowercase);
        let mut got: Vec<Told> = at(payload, &[of, "pull_requests"])
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| {
                let url = at(p, &["base", "repo", "url"]).and_then(Value::as_str);
                repo.as_deref()
                    .zip(url)
                    .is_some_and(|(r, u)| u.to_lowercase().ends_with(&format!("/repos/{r}")))
            })
            .filter_map(|p| p.get("number").and_then(Value::as_u64).map(Told::Pr))
            .collect();
        if got.is_empty() {
            got.extend(
                text(head)
                    .filter(|b| branch(b))
                    .map(|b| Told::Branch(b.to_owned())),
            );
        }
        if got.is_empty() {
            got.extend(
                text(&[of, "head_sha"])
                    .filter(|c| commit(c))
                    .map(|c| Told::Commit(c.to_owned())),
            );
        }
        got
    };
    let got = match event {
        "pull_request"
        | "pull_request_review"
        | "pull_request_review_comment"
        | "pull_request_review_thread" => number(&["pull_request", "number"])
            .map(Told::Pr)
            .into_iter()
            .collect(),
        // A comment on a PR is an issue_comment whose issue is the PR.
        "issue_comment" if at(payload, &["issue", "pull_request"]).is_some() => {
            number(&["issue", "number"])
                .map(Told::Pr)
                .into_iter()
                .collect()
        }
        "check_suite" => checked("check_suite", &["check_suite", "head_branch"]),
        // One only starting says nothing yet: its suite's end is what a
        // pass waits for, and a run's own end only says red early.
        "check_run" if payload.get("action").and_then(Value::as_str) == Some("completed") => {
            checked("check_run", &["check_run", "check_suite", "head_branch"])
        }
        // The commit: its `branches` are every branch it's on, the default
        // one too once it's merged.
        "status" => text(&["sha"])
            .filter(|c| commit(c))
            .map(|c| Told::Commit(c.to_owned()))
            .into_iter()
            .collect(),
        "push" => {
            let pushed = text(&["ref"]).and_then(|r| r.strip_prefix("refs/heads/"));
            let default = text(&["repository", "default_branch"]);
            let deleted = payload
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match pushed {
                Some(b) if !deleted && Some(b) != default && branch(b) => {
                    vec![Told::Branch(b.to_owned())]
                }
                _ => vec![],
            }
        }
        _ => vec![],
    };
    (!got.is_empty()).then_some(got)
}

/// One of the PRs whose events go first, as the handler's passes list them
/// (`firsts`): its number, head branch and head commit.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct First {
    pub number: u64,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
}

impl First {
    pub fn matches(&self, t: &Told) -> bool {
        match t {
            Told::Pr(n) => *n == self.number,
            Told::Branch(b) => self.branch.as_deref() == Some(b.as_str()),
            Told::Commit(c) => self
                .head
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(c)),
            Told::Issue(_) => false,
        }
    }
}

/// The firsts file, {"OWNER/REPO": [First...]}, by repo in lower case; none
/// when it's missing or isn't that.
pub fn firsts(v: &Value) -> BTreeMap<String, Vec<First>> {
    let Value::Object(m) = v else {
        return BTreeMap::new();
    };
    m.iter()
        .filter(|(k, _)| burrow_github::repo_name(k))
        .filter_map(|(k, v)| Some((k.to_lowercase(), serde_json::from_value(v.clone()).ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{First, Told, firsts, prs};

    fn pr(n: u64) -> Told {
        Told::Pr(n)
    }

    // Each kind of event names what it's about, a line of the file each.
    #[test]
    fn lines() {
        for t in [
            Told::Issue(7),
            pr(7),
            Told::Branch("fix/thing".into()),
            Told::Commit("abc1234".into()),
        ] {
            assert_eq!(Told::parse(&t.to_string()), Some(t));
        }
        for bad in [
            "",
            "pr",
            "pr x",
            "branch ",
            "branch a b",
            "commit xyz",
            "commit 12",
            "tag v1",
            "-3",
        ] {
            assert_eq!(Told::parse(bad), None, "{bad}");
        }
    }

    // What each event the steward takes is about: a PR's own events and
    // comments, its checks (by PR when GitHub lists it in this repo, else
    // branch, else commit), a status's commit and a push's branch. A push
    // to the default branch, a deleted branch, an issue's comment or the
    // repo's own event are about no one PR.
    #[test]
    fn deliveries() {
        let repo = json!({"full_name": "Sam/R", "default_branch": "main"});
        let with = |extra: serde_json::Value| {
            let mut p = json!({"repository": repo});
            p.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            p
        };
        let here = json!({"number": 4, "base": {"repo": {"name": "r", "url": "https://api.github.com/repos/sam/r"}}});
        let fork = json!({"number": 9, "base": {"repo": {"name": "r", "url": "https://api.github.com/repos/kim/r"}}});
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let cases = [
            (
                "pull_request",
                with(json!({"action": "labeled", "pull_request": {"number": 4}})),
                Some(vec![pr(4)]),
            ),
            (
                "pull_request_review",
                with(json!({"pull_request": {"number": 4}})),
                Some(vec![pr(4)]),
            ),
            (
                "pull_request_review_comment",
                with(json!({"pull_request": {"number": 4}})),
                Some(vec![pr(4)]),
            ),
            (
                "pull_request_review_thread",
                with(json!({"pull_request": {"number": 4}})),
                Some(vec![pr(4)]),
            ),
            (
                "issue_comment",
                with(json!({"issue": {"number": 4, "pull_request": {}}})),
                Some(vec![pr(4)]),
            ),
            ("issue_comment", with(json!({"issue": {"number": 4}})), None),
            (
                "check_suite",
                with(
                    json!({"check_suite": {"pull_requests": [here, fork], "head_branch": "fix", "head_sha": sha}}),
                ),
                Some(vec![pr(4)]),
            ),
            (
                "check_suite",
                with(
                    json!({"check_suite": {"pull_requests": [fork], "head_branch": "fix", "head_sha": sha}}),
                ),
                Some(vec![Told::Branch("fix".into())]),
            ),
            (
                "check_suite",
                with(
                    json!({"check_suite": {"pull_requests": [], "head_branch": null, "head_sha": sha}}),
                ),
                Some(vec![Told::Commit(sha.into())]),
            ),
            (
                "check_run",
                with(
                    json!({"action": "completed", "check_run": {"pull_requests": [here], "head_sha": sha, "check_suite": {"head_branch": "fix"}}}),
                ),
                Some(vec![pr(4)]),
            ),
            (
                "check_run",
                with(
                    json!({"action": "created", "check_run": {"pull_requests": [here], "head_sha": sha, "check_suite": {"head_branch": "fix"}}}),
                ),
                None,
            ),
            (
                "check_run",
                with(
                    json!({"action": "completed", "check_run": {"pull_requests": [], "head_sha": sha, "check_suite": {"head_branch": "fix"}}}),
                ),
                Some(vec![Told::Branch("fix".into())]),
            ),
            (
                "status",
                with(json!({"sha": sha, "branches": [{"name": "main"}, {"name": "fix"}]})),
                Some(vec![Told::Commit(sha.into())]),
            ),
            (
                "push",
                with(json!({"ref": "refs/heads/fix"})),
                Some(vec![Told::Branch("fix".into())]),
            ),
            ("push", with(json!({"ref": "refs/heads/main"})), None),
            (
                "push",
                with(json!({"ref": "refs/heads/fix", "deleted": true})),
                None,
            ),
            ("push", with(json!({"ref": "refs/tags/v1"})), None),
            ("repository", with(json!({"action": "renamed"})), None),
            ("issues", with(json!({"issue": {"number": 4}})), None),
        ];
        for (event, payload, want) in cases {
            assert_eq!(prs(event, &payload), want, "{event} {payload}");
        }
    }

    // A first PR is matched by its number, its head branch or its head
    // commit; the file's repos are case-blind, and anything odd is dropped.
    #[test]
    fn first_prs() {
        let got = firsts(&json!({
            "Sam/R": [{"number": 4, "branch": "fix", "head": "ABCDEF1"}],
            "--bad": [{"number": 1}],
            "sam/q": "nope",
        }));
        assert_eq!(got.keys().collect::<Vec<_>>(), ["sam/r"]);
        let f: &First = &got["sam/r"][0];
        assert!(
            f.matches(&pr(4))
                && f.matches(&Told::Branch("fix".into()))
                && f.matches(&Told::Commit("abcdef1".into()))
        );
        assert!(
            !f.matches(&pr(5))
                && !f.matches(&Told::Branch("main".into()))
                && !f.matches(&Told::Issue(4))
        );
        assert!(firsts(&json!([1])).is_empty());
    }
}
