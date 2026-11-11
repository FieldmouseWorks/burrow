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
}
