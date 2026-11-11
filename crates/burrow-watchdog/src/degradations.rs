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

/// A PR run's text is the PR author's: only a run whose original actor (a
/// rerun keeps it; `triggering_actor` is whoever clicked) is a configured
/// author, from a branch of this repository, may supply keys and links.
/// Unknown provenance is untrusted.
pub fn trusted(run: &serde_json::Value, repo: &str, authors: &[String]) -> bool {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let from_here = run["head_repository"]["full_name"]
        .as_str()
        .is_some_and(|n| same(n, repo));
    from_here
        && run["actor"]["login"]
            .as_str()
            .is_some_and(|a| authors.iter().any(|x| same(x, a)))
}

/// The current result of every logical job across attempts: a partial rerun
/// keeps the jobs it didn't rerun and replaces the ones it did. Jobs are
/// matched by name (matrix legs have their own), newest attempt wins.
pub fn latest_jobs(all: &[serde_json::Value]) -> serde_json::Value {
    let mut latest: BTreeMap<&str, &serde_json::Value> = BTreeMap::new();
    for job in all {
        let name = job["name"].as_str().unwrap_or_default();
        let attempt = |j: &serde_json::Value| j["run_attempt"].as_u64().unwrap_or(0);
        if latest
            .get(name)
            .is_none_or(|old| attempt(job) >= attempt(old))
        {
            latest.insert(name, job);
        }
    }
    serde_json::json!({ "jobs": latest.into_values().collect::<Vec<_>>() })
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
    fn job(name: &str, attempt: u64, step: Option<&str>) -> serde_json::Value {
        let steps: Vec<_> = step
            .into_iter()
            .map(|n| serde_json::json!({"name":n,"conclusion":"success"}))
            .collect();
        serde_json::json!({"name":name,"run_attempt":attempt,"steps":steps})
    }
    #[test]
    fn partial_reruns_keep_unrerun_jobs_and_replace_rerun_ones() {
        // Leg a degraded and passed in attempt 1; leg b failed and was rerun.
        let all = [
            job("a", 1, Some("degraded/cache")),
            job("b", 1, None),
            job("b", 2, None),
        ];
        assert_eq!(keys(&latest_jobs(&all)), BTreeSet::from(["cache".into()]));
        // Order doesn't matter; a rerun leg that recovered drops its signal.
        let all = [
            job("b", 2, None),
            job("b", 1, Some("degraded/save")),
            job("a", 1, Some("degraded/cache")),
            job("a", 3, None),
        ];
        assert!(keys(&latest_jobs(&all)).is_empty());
        // A full rerun reporting something new.
        let all = [job("a", 1, None), job("a", 2, Some("degraded/new"))];
        assert_eq!(keys(&latest_jobs(&all)), BTreeSet::from(["new".into()]));
    }
    #[test]
    fn only_configured_authors_on_this_repo_are_trusted() {
        let authors = ["Sam".to_string()];
        let r = |actor: serde_json::Value, head: serde_json::Value| {
            serde_json::json!({"actor":{"login":actor},"head_repository":{"full_name":head},
                "triggering_actor":{"login":"sam"}})
        };
        assert!(trusted(&r("sam".into(), "o/r".into()), "O/R", &authors));
        // Sam rerunning an outsider's run: the actor stays the outsider.
        assert!(!trusted(&r("eve".into(), "o/r".into()), "o/r", &authors));
        // A fork, even by a configured author's login.
        assert!(!trusted(&r("sam".into(), "eve/r".into()), "o/r", &authors));
        // Unknown provenance.
        assert!(!trusted(&serde_json::json!({}), "o/r", &authors));
        assert!(!trusted(
            &r("sam".into(), serde_json::Value::Null),
            "o/r",
            &authors
        ));
    }
}
