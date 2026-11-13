//! Runs queued without a single job, and the replacement work that discharges them.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::ci::text;
use crate::ci_runs::{FLAKE, LANE, job, judged, suite, suite_flow, workflow};

/// A queued run with zero jobs past fifteen minutes is stranded, rather than slow.
pub const AFTER: f64 = 15.0 * 60.0;

fn idle(s: &Value) -> bool {
    ["QUEUED", "REQUESTED", "PENDING"]
        .iter()
        .any(|status| s["status"] == *status)
        && s["checkRuns"]["totalCount"].as_u64() == Some(0)
}

fn flow(s: &Value) -> Option<&str> {
    suite_flow(s).or_else(|| text(&s["workflowRun"]["workflow"]["name"]))
}

fn ran(c: &Value) -> bool {
    c["status"] == "COMPLETED" && c["conclusion"] != "SKIPPED" && c["conclusion"] != "CANCELLED"
}

fn covers(o: &Value, s: &Value, stale: &BTreeSet<String>, contexts: &[Value], base: &str) -> bool {
    if std::ptr::eq(o, s)
        || stale.contains(text(&o["id"]).unwrap_or(""))
        || idle(o)
        || flow(o) != flow(s)
        || flow(s).is_none()
    {
        return false;
    }
    if o["status"] != "COMPLETED" {
        return o["checkRuns"]["totalCount"].as_u64().unwrap_or(1) > 0;
    }
    let mine: Vec<_> = contexts
        .iter()
        .filter(|c| c["__typename"] == "CheckRun" && suite(c)["id"] == o["id"] && ran(c))
        .collect();
    if suite_flow(o) != Some(FLAKE) {
        return !mine.is_empty();
    }
    // Every shared call owes its own gate or current-diff docs work. A
    // lane, a matrix fragment, or another base's suite cannot discharge it.
    let calls: BTreeSet<_> = contexts
        .iter()
        .filter(|c| {
            workflow(c) == Some(FLAKE)
                && !stale.contains(text(&suite(c)["id"]).unwrap_or(""))
                && job(c).1.starts_with(LANE)
        })
        .map(|c| job(c).0)
        .collect();
    let laned: BTreeSet<_> = mine
        .iter()
        .filter(|c| judged(c) == Some((base, None)))
        .map(|c| job(c).0)
        .collect();
    let worked: BTreeSet<_> = mine
        .iter()
        .filter(|c| job(c).1 == "check" || judged(c).is_some_and(|(_, f)| f.is_some()))
        .map(|c| job(c).0)
        .collect();
    !calls.is_empty() && calls.is_subset(&laned) && calls.is_subset(&worked)
}

pub fn suites<'a>(
    suites: &'a [Value],
    stale: &BTreeSet<String>,
    contexts: &[Value],
    base: &str,
    now: f64,
) -> Vec<&'a Value> {
    suites
        .iter()
        .filter(|s| {
            idle(s)
                && crate::ci::suite_run_id(s).is_some()
                && text(&s["workflowRun"]["createdAt"])
                    .and_then(|t| crate::ts(t).ok())
                    .is_some_and(|made| now - made > AFTER)
                && !suites.iter().any(|o| covers(o, s, stale, contexts, base))
        })
        .collect()
}
