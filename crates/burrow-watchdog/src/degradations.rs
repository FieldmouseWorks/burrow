//! Reusable Actions degradation policy. A successful `degraded/KEY` step
//! is an explicit signal; skipped reporters are healthy. Count runs, never
//! matrix legs or reruns, and isolate workflows and repositories.
use crate::watch::{File, Item};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Run {
    pub id: u64,
    pub at: f64,
    pub link: String,
    pub keys: BTreeSet<String>,
}

/// `defaults` contains the latest two completed default-branch push runs,
/// newest first; `prs` the completed PR runs created in the last day.
/// Preserve a stall's start across passes so the ladder keeps its progress.
pub fn evaluate(
    scope: &str,
    defaults: &[Run],
    prs: &[Run],
    threshold: usize,
    now: f64,
    previous: &File,
) -> File {
    let mut counts: BTreeMap<String, BTreeMap<u64, &Run>> = BTreeMap::new();
    for run in prs.iter().filter(|r| r.at >= now - 86400.0) {
        for key in &run.keys {
            counts.entry(key.clone()).or_default().insert(run.id, run);
        }
    }
    let mut active: BTreeMap<String, &Run> = BTreeMap::new();
    for (key, runs) in counts {
        if runs.len() >= threshold {
            active.insert(
                key,
                runs.into_values()
                    .max_by(|a, b| a.at.total_cmp(&b.at))
                    .unwrap(),
            );
        }
    }
    if let [new, old, ..] = defaults {
        for key in new.keys.intersection(&old.keys) {
            active.insert(key.clone(), new);
        }
    }
    let items = active
        .into_iter()
        .map(|(key, run)| {
            let name = format!("{scope}/{key}");
            let since = previous.items.get(&name).map_or(now, |i| i.since);
            let item = Item {
                what: format!("{scope}: degraded/{key}"),
                phase: "repeated workflow degradation".into(),
                since,
                due: since,
                link: Some(run.link.clone()),
                kick: Some("degradations".into()),
                ..Item::default()
            };
            (name, item)
        })
        .collect();
    File { at: now, items }
}

/// GitHub's run-to-PR association authenticates PR identity, while
/// the PR resource supplies its author. Event actors are not authors. Check
/// this before consulting cached observations too. A PR may since have moved
/// on: its recorded run snapshot remains associated with that same PR.
pub fn trusted(
    run: &serde_json::Value,
    pr: &serde_json::Value,
    repo: &str,
    authors: &[String],
) -> bool {
    let same_repo =
        |v: &serde_json::Value| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(repo));
    let Some(associations) = run["pull_requests"].as_array() else {
        return false;
    };
    let [association] = associations.as_slice() else {
        return false;
    };
    let sha = run["head_sha"]
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let head_id = pr["head"]["repo"]["id"].as_u64();
    same_repo(&run["head_repository"]["full_name"])
        && same_repo(&pr["head"]["repo"]["full_name"])
        && pr["user"]["login"]
            .as_str()
            .is_some_and(|a| authors.iter().any(|x| x.eq_ignore_ascii_case(a)))
        && pr["number"]
            .as_u64()
            .is_some_and(|n| association["number"].as_u64() == Some(n))
        // The association's head SHA is mutable: it follows later pushes,
        // unlike the run's head_sha. Authenticate the immutable PR ID instead.
        && sha.is_some()
        && pr["id"].as_u64().is_some_and(|id| association["id"].as_u64() == Some(id))
        && head_id.is_some_and(|id| {
            association["head"]["repo"]["id"].as_u64() == Some(id)
                && run["head_repository"]["id"].as_u64() == Some(id)
        })
}

/// Parse only explicit reporter steps. A failed reporter still signals the
/// degradation it was trying to report; cancelled and skipped ones don't.
pub fn keys(jobs: &serde_json::Value) -> BTreeSet<String> {
    jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|j| j["steps"].as_array().into_iter().flatten())
        .filter(|s| matches!(s["conclusion"].as_str(), Some("success" | "failure")))
        .filter_map(|s| s["name"].as_str()?.strip_prefix("degraded/"))
        .filter(|key| {
            !key.is_empty()
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(id: u64, key: bool) -> Run {
        Run {
            id,
            at: 100000.0,
            link: format!("https://example.org/{id}"),
            keys: if key {
                BTreeSet::from(["cache".into()])
            } else {
                BTreeSet::new()
            },
        }
    }
    #[test]
    fn repeated_defaults_and_recovery() {
        let empty = File::default();
        assert!(
            evaluate("sam/repo/ci", &[run(2, true)], &[], 3, 100001.0, &empty)
                .items
                .is_empty()
        );
        let found = evaluate(
            "sam/repo/ci",
            &[run(2, true), run(1, true)],
            &[],
            3,
            100001.0,
            &empty,
        );
        assert_eq!(found.items.len(), 1);
        let again = evaluate(
            "sam/repo/ci",
            &[run(3, true), run(2, true)],
            &[],
            3,
            100100.0,
            &found,
        );
        assert_eq!(again.items.values().next().unwrap().since, 100001.0);
        assert!(
            evaluate(
                "sam/repo/ci",
                &[run(3, false), run(2, true)],
                &[],
                3,
                100100.0,
                &found
            )
            .items
            .is_empty()
        );
    }
    #[test]
    fn prs_count_distinct_runs_in_a_day_and_expire() {
        let empty = File::default();
        let prs = [run(1, true), run(1, true), run(2, true)];
        assert!(
            evaluate("scope", &[], &prs, 3, 100001.0, &empty)
                .items
                .is_empty()
        );
        let prs = [run(1, true), run(2, true), run(3, true)];
        assert_eq!(
            evaluate("scope", &[], &prs, 3, 100001.0, &empty)
                .items
                .len(),
            1
        );
        assert!(
            evaluate("scope", &[], &prs, 3, 200000.0, &empty)
                .items
                .is_empty()
        );
    }
    #[test]
    fn api_reporters_not_warnings_or_skips() {
        let jobs = serde_json::json!({"jobs":[{"steps":[
            {"name":"degraded/cache", "conclusion":"success"},
            {"name":"degraded/save", "conclusion":"failure"},
            {"name":"degraded/skip", "conclusion":"skipped"},
            {"name":"cache warning", "conclusion":"success"}]}]});
        assert_eq!(keys(&jobs), BTreeSet::from(["cache".into(), "save".into()]));
    }
    #[test]
    fn current_jobs_preserve_duplicate_names_and_ignore_display_identity() {
        let jobs = serde_json::json!({"jobs":[
            {"id":1,"name":"check","steps":[{"name":"degraded/cache","conclusion":"success"}]},
            {"id":2,"name":"check","steps":[]}
        ]});
        assert_eq!(keys(&jobs), BTreeSet::from(["cache".into()]));
        // GitHub supplies only current executions, even if a name changed.
        assert!(
            keys(&serde_json::json!({"jobs":[{"id":3,"name":"check attempt 2","steps":[]}]}))
                .is_empty()
        );
    }
    #[test]
    fn author_and_observed_head_are_authenticated_not_event_actors() {
        let sha = "a".repeat(40);
        let run = serde_json::json!({"head_sha":sha,"actor":{"login":"outsider"},
            "triggering_actor":{"login":"sam"},"head_repository":{"id":7,"full_name":"o/r"},
            "pull_requests":[{"id":1003,"number":3,"head":{"sha":"b".repeat(40),"repo":{"id":7}}}]});
        let pr = serde_json::json!({"id":1003,"number":3,"user":{"login":"sam"},
            "head":{"sha":"b".repeat(40),"repo":{"id":7,"full_name":"o/r"}}});
        let authors = ["Sam".into()];
        assert!(trusted(&run, &pr, "O/R", &authors));
        let mut outsider = pr.clone();
        outsider["user"]["login"] = "eve".into();
        assert!(!trusted(&run, &outsider, "o/r", &authors));
        let mut reopened = run.clone();
        reopened["actor"]["login"] = "sam".into();
        assert!(!trusted(&reopened, &outsider, "o/r", &authors));
        for pointer in [
            "/head_sha",
            "/pull_requests/0/id",
            "/head_repository/id",
            "/pull_requests",
            "/pull_requests/0/number",
        ] {
            let mut unknown = run.clone();
            *unknown.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(!trusted(&unknown, &pr, "o/r", &authors), "{pointer}");
        }
        let mut fork = pr.clone();
        fork["head"]["repo"]["full_name"] = "eve/r".into();
        assert!(!trusted(&run, &fork, "o/r", &authors));
    }
}
