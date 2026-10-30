//! A handler as modules/events.nix writes it, and which deliveries start its
//! passes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::glob;
use crate::line::Pass;

/// The service's config, the JSON file EVENTS_CONFIG names.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Where deliveries come in from nginx; none without a host.
    pub port: Option<u16>,
    pub kick_port: u16,
    pub handlers: BTreeMap<String, Handler>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handler {
    pub events: Vec<String>,
    pub unit: String,
    #[serde(default)]
    pub wake: Option<PathBuf>,
    #[serde(default)]
    pub own_statuses: Vec<String>,
    #[serde(default = "yes")]
    pub per_repo: bool,
    #[serde(default)]
    pub repos: Option<Vec<String>>,
    #[serde(default)]
    pub pushes: Pushes,
    #[serde(default)]
    pub branches: Option<Vec<String>>,
    #[serde(default)]
    pub issue_titles: Option<Vec<String>>,
    /// Seconds one of its passes starts after the last, at least.
    #[serde(default = "gap")]
    pub gap: f64,
}

fn yes() -> bool {
    true
}

fn gap() -> f64 {
    crate::line::GAP
}

/// Which pushes count: to any branch, or only to the repo's default branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pushes {
    #[default]
    Any,
    Default,
}

impl Handler {
    /// Whether the handler takes this repo: one of its `repos`, case-blind,
    /// or any repo when it names none.
    pub fn covers(&self, repo: &str) -> bool {
        match &self.repos {
            None => true,
            Some(names) => {
                let repo = repo.to_lowercase();
                names.iter().any(|n| n.to_lowercase() == repo)
            }
        }
    }

    /// Whether this delivery (its repo already checked) starts the
    /// handler's pass.
    pub fn takes(&self, event: &str, payload: &Value) -> bool {
        let repo = at(payload, &["repository", "full_name"]).and_then(Value::as_str);
        if !self.events.iter().any(|e| e == event) || !repo.is_some_and(|r| self.covers(r)) {
            return false;
        }
        if event == "status"
            && let Some(context) = payload.get("context").and_then(Value::as_str)
            && self.own_statuses.iter().any(|s| s == context)
        {
            return false;
        }
        if event == "push" {
            return match self.pushes {
                Pushes::Any => true,
                Pushes::Default => {
                    let default =
                        at(payload, &["repository", "default_branch"]).and_then(Value::as_str);
                    let pushed = payload.get("ref").and_then(Value::as_str);
                    default.is_some_and(|d| pushed == Some(&format!("refs/heads/{d}")))
                }
            };
        }
        if event == "issues"
            && let Some(titles) = &self.issue_titles
        {
            let title = at(payload, &["issue", "title"]).and_then(Value::as_str);
            return title.is_some_and(|t| titles.iter().any(|p| glob::matches(p, t)));
        }
        let Some(patterns) = &self.branches else {
            return true;
        };
        match branches(event, payload) {
            None => true,
            Some(names) => names
                .iter()
                .any(|n| patterns.iter().any(|p| glob::matches(p, n))),
        }
    }

    /// Where a repo's event goes in the handler's line: the repo's own pass,
    /// or its full pass when it has none over one repo.
    pub fn place(&self, repo: &str) -> Pass {
        if self.per_repo {
            Pass::repo(repo)
        } else {
            Pass::Full
        }
    }

    /// The unit that is this pass.
    pub fn unit(&self, pass: &Pass) -> String {
        match pass {
            Pass::Full => format!("{}.service", self.unit),
            Pass::Repo(repo) => format!("{}@{}.service", self.unit, escape(repo)),
        }
    }
}

fn at<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().try_fold(v, |v, k| v.get(k))
}

/// The branches a delivery other than a push is about: a pull request's
/// head, the commit's whose checks or status it reports. None: it's about no
/// branch (an issue, a comment).
pub fn branches(event: &str, payload: &Value) -> Option<Vec<String>> {
    let named = |keys: &[&str]| {
        at(payload, keys)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect()
    };
    Some(if event.starts_with("pull_request") {
        named(&["pull_request", "head", "ref"])
    } else if event == "check_suite" {
        named(&["check_suite", "head_branch"])
    } else if event == "check_run" {
        named(&["check_run", "check_suite", "head_branch"])
    } else if event == "workflow_run" {
        named(&["workflow_run", "head_branch"])
    } else if event == "status" {
        payload
            .get("branches")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| b.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect()
    } else {
        return None;
    })
}

/// systemd-escape: a repo as a unit's instance name (%I gives it back).
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c == '/' {
            out.push('-');
        } else if c.is_ascii_alphanumeric()
            || (matches!(c, ':' | '_' | '.') && !(i == 0 && c == '.'))
        {
            out.push(c);
        } else {
            let mut buf = [0; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("\\x{b:02x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Handler, Pushes, escape};
    use crate::line::Pass;

    fn handler(events: &[&str], unit: &str) -> Handler {
        Handler {
            events: events.iter().map(|e| e.to_string()).collect(),
            unit: unit.into(),
            per_repo: true,
            ..Handler::default()
        }
    }

    fn names(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|s| s.to_string()).collect())
    }

    fn payload(repo: &str, extra: Value) -> Value {
        let mut p = json!({"repository": {"full_name": repo, "default_branch": "main"}});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        p
    }

    // A repo is a unit's instance as systemd-escape has it, so %I gives it back.
    #[test]
    fn units() {
        assert_eq!(escape("sam/r"), "sam-r");
        assert_eq!(escape("sam/my-repo"), "sam-my\\x2drepo");
        assert_eq!(escape(".a/b_c.d"), "\\x2ea-b_c.d");
        assert_eq!(escape("a/é"), "a-\\xc3\\xa9");
        let h = handler(&["push"], "autoland");
        assert_eq!(h.unit(&Pass::repo("sam/r")), "autoland@sam-r.service");
        assert_eq!(h.unit(&Pass::Full), "autoland.service");
    }

    // A handler takes the repos it names, case-blind: none named, every repo;
    // [], none. A name is a name, never a pattern.
    #[test]
    fn covers() {
        let mut h = handler(&[], "u");
        assert!(h.covers("sam/r"));
        h.repos = Some(vec![]);
        assert!(!h.covers("sam/r"));
        h.repos = names(&["Sam/R", "kim/r"]);
        assert_eq!(
            ["sam/r", "KIM/R", "sam/q"].map(|r| h.covers(r)),
            [true, true, false]
        );
        h.repos = names(&["sam/*"]);
        assert!(!h.covers("sam/r"));
    }

    // Which deliveries start a handler's pass: its events, its repos, pushes
    // to the default branch only if it says so, and the rest only about its
    // branches if it names some (an event about no branch still counts).
    #[test]
    fn picky() {
        let mut h = handler(
            &[
                "push",
                "pull_request",
                "check_suite",
                "check_run",
                "status",
                "issues",
            ],
            "renovate",
        );
        h.repos = names(&["sam/r"]);
        h.pushes = Pushes::Default;
        h.branches = names(&["renovate/*"]);
        let cases = [
            (
                "push",
                payload("sam/r", json!({"ref": "refs/heads/main"})),
                true,
            ),
            (
                "push",
                payload("sam/r", json!({"ref": "refs/heads/renovate/x"})),
                false,
            ),
            (
                "push",
                payload("sam/r", json!({"ref": "refs/tags/main"})),
                false,
            ),
            (
                "push",
                payload("kim/r", json!({"ref": "refs/heads/main"})),
                false,
            ),
            (
                "push",
                json!({"repository": {"full_name": "sam/r"}, "ref": "refs/heads/None"}),
                false,
            ),
            (
                "pull_request",
                payload(
                    "sam/r",
                    json!({"pull_request": {"head": {"ref": "renovate/x"}}}),
                ),
                true,
            ),
            (
                "pull_request",
                payload("sam/r", json!({"pull_request": {"head": {"ref": "fix"}}})),
                false,
            ),
            (
                "check_suite",
                payload(
                    "sam/r",
                    json!({"check_suite": {"head_branch": "renovate/x"}}),
                ),
                true,
            ),
            (
                "check_suite",
                payload("sam/r", json!({"check_suite": {"head_branch": "main"}})),
                false,
            ),
            (
                "check_suite",
                payload("sam/r", json!({"check_suite": {"head_branch": null}})),
                false,
            ),
            (
                "check_run",
                payload(
                    "sam/r",
                    json!({"check_run": {"check_suite": {"head_branch": "renovate/x"}}}),
                ),
                true,
            ),
            (
                "status",
                payload(
                    "sam/r",
                    json!({"branches": [{"name": "fix"}, {"name": "renovate/x"}]}),
                ),
                true,
            ),
            (
                "status",
                payload("sam/r", json!({"branches": [{"name": "fix"}]})),
                false,
            ),
            (
                "status",
                payload("sam/r", json!({"branches": "odd"})),
                false,
            ),
            ("issues", payload("sam/r", json!({})), true),
            ("issue_comment", payload("sam/r", json!({})), false),
        ];
        for (event, p, want) in cases {
            assert_eq!(h.takes(event, &p), want, "{event} {p}");
        }
        // A handler that asks for none of that takes any push, and every branch.
        let plain = handler(&["push", "check_suite"], "autoland");
        assert!(plain.takes("push", &payload("sam/r", json!({"ref": "refs/heads/fix"}))));
        assert!(plain.takes(
            "check_suite",
            &payload("sam/r", json!({"check_suite": {"head_branch": "fix"}}))
        ));
    }

    // One that names issue titles takes an issue's event only for one that
    // matches one of those globs, case and all; a comment, or an event about
    // a branch, is no issue's.
    #[test]
    fn titled() {
        let mut h = handler(&["issues", "issue_comment", "push"], "autoland");
        h.issue_titles = names(&["Dependency Dashboard", "flaky: *"]);
        let cases = [
            (
                "issues",
                payload("sam/r", json!({"issue": {"title": "Dependency Dashboard"}})),
                true,
            ),
            (
                "issues",
                payload(
                    "sam/r",
                    json!({"issue": {"title": "Dependency Dashboard (old)"}}),
                ),
                false,
            ),
            (
                "issues",
                payload("sam/r", json!({"issue": {"title": "dependency dashboard"}})),
                false,
            ),
            (
                "issues",
                payload(
                    "sam/r",
                    json!({"issue": {"title": "flaky: browser/stream[2]"}}),
                ),
                true,
            ),
            (
                "issues",
                payload(
                    "sam/r",
                    json!({"issue": {"title": "Flaky: browser/stream"}}),
                ),
                false,
            ),
            (
                "issues",
                payload("sam/r", json!({"issue": {"title": "not flaky: x"}})),
                false,
            ),
            (
                "issues",
                payload("sam/r", json!({"issue": {"title": null}})),
                false,
            ),
            ("issues", payload("sam/r", json!({"issue": ["odd"]})), false),
            ("issues", payload("sam/r", json!({})), false),
            (
                "issue_comment",
                payload("sam/r", json!({"issue": {"title": "Something else"}})),
                true,
            ),
            (
                "push",
                payload("sam/r", json!({"ref": "refs/heads/main"})),
                true,
            ),
        ];
        for (event, p, want) in cases {
            assert_eq!(h.takes(event, &p), want, "{event} {p}");
        }
    }

    // The steward's own status is no news to it; another context is.
    #[test]
    fn own_statuses() {
        let mut h = handler(&["status"], "autoland");
        h.own_statuses = vec!["review-settled".into()];
        assert!(!h.takes(
            "status",
            &payload("sam/r", json!({"context": "review-settled"}))
        ));
        assert!(h.takes("status", &payload("sam/r", json!({"context": "proof"}))));
        assert!(h.takes("status", &payload("sam/r", json!({}))));
    }

    // One with no pass over one repo gets its full pass for a repo's event.
    #[test]
    fn place() {
        let mut h = handler(&["push"], "autoland");
        assert_eq!(h.place("Sam/R"), Pass::repo("sam/r"));
        h.per_repo = false;
        assert_eq!(h.place("sam/r"), Pass::Full);
    }

    // The config as modules/events.nix writes it, nulls and all.
    #[test]
    fn config() {
        let cfg: super::Config = serde_json::from_value(json!({
            "kickPort": 8788, "port": null,
            "handlers": {"update": {"events": ["push"], "unit": "remi-update", "wake": null, "ownStatuses": [],
                                    "perRepo": false, "repos": ["sam/r"], "pushes": "default", "branches": null,
                                    "issueTitles": null}}
        }))
        .unwrap();
        let h = &cfg.handlers["update"];
        assert_eq!(
            (cfg.port, cfg.kick_port, h.per_repo, h.pushes),
            (None, 8788, false, Pushes::Default)
        );
        assert!(
            serde_json::from_value::<super::Handler>(
                json!({"events": [], "unit": "u", "pushes": "some"})
            )
            .is_err()
        );
    }
}
