//! The head's one CI verdict and cancellation recovery owner. The steward
//! hands over GraphQL nodes and gets one verdict; all remote reads and
//! reruns use its inherited App token. GitHub's run_attempt is the only
//! retry store, shared across PRs, process restarts and uncertain writes.

use std::collections::{BTreeMap, BTreeSet};

use burrow_github::{Error, call, pages};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ci_runs::{self, After, cancelled, suite, workflow, workflow_name};
use crate::pass::Pass;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub pr: Value,
    #[serde(default)]
    pub proofs: Vec<String>,
    pub moved: Option<Vec<String>>,
    /// A caller without a repository asks only about the supplied nodes.
    pub remote: bool,
    /// A pass recovers cancelled runs; a pure classification doesn't.
    pub recover: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Verdict {
    pub state: &'static str,
    pub names: Vec<String>,
    pub docs: bool,
}

pub(crate) fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}
pub(crate) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn run_id(c: &Value) -> Option<String> {
    let id = &suite(c)["workflowRun"]["databaseId"];
    text(id)
        .map(String::from)
        .or_else(|| id.as_u64().filter(|i| *i > 0).map(|i| i.to_string()))
}

fn rerun_key(c: &Value) -> Option<String> {
    c["databaseId"]
        .as_u64()
        .filter(|i| *i > 0)
        .and_then(|_| run_id(c))
}

#[derive(Default)]
struct Recovery {
    runs: BTreeMap<String, Run>,
    refused: BTreeSet<String>,
    lookup: BTreeSet<String>,
    due: BTreeSet<String>,
}

#[derive(Deserialize)]
struct Run {
    run_attempt: u64,
    status: String,
    conclusion: Option<String>,
}

/// Read a complete verdict, recover each eligible workflow once, and read
/// its current attempt back. An unanswered read/write fails the step with
/// its typed retry classification intact; a refused write is retried on
/// the next pass, with Actions permission failures visible in the verdict.
pub fn check(p: &mut Pass, ask: &Ask) -> Result<Verdict, Error> {
    let commit = &ask.pr["commits"]["nodes"][0]["commit"];
    let contexts = &commit["statusCheckRollup"]["contexts"];
    let suites = &commit["checkSuites"];
    if [contexts, suites]
        .iter()
        .any(|x| x["pageInfo"]["hasNextPage"] == true)
    {
        return Ok(Verdict {
            state: "pending",
            names: vec!["over 100 checks, too many to read".into()],
            docs: false,
        });
    }
    let contexts = array(&contexts["nodes"]);
    let runs = ci_runs::runs(p, ask, contexts);
    let mut timed_out = BTreeSet::new();
    for c in contexts.iter().filter(|c| {
        cancelled(c)
            && !runs.stale.contains(text(&suite(c)["id"]).unwrap_or(""))
            && ci_runs::after(c, contexts, &runs.stale) == After::None
    }) {
        if ask.remote
            && let Some(id) = c["databaseId"].as_u64().filter(|id| *id > 0)
        {
            let path = format!("repos/{}/check-runs/{id}/annotations?per_page=100", p.repo);
            let pages = pages(p.api, &path).map_err(|e| p.noted(e))?;
            if pages.iter().flat_map(array).any(|a| {
                text(&a["message"])
                    .is_some_and(|m| m.contains("has exceeded the maximum execution time"))
            }) {
                timed_out.insert(id);
            }
        }
    }
    let mut recovery = Recovery::default();
    let got = classify(
        p,
        ask,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &timed_out,
        &mut recovery,
    );
    if !ask.recover || !ask.remote || recovery.lookup.is_empty() {
        return Ok(got);
    }
    read(p, &mut recovery)?;
    let got = classify(
        p,
        ask,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &timed_out,
        &mut recovery,
    );
    if p.dry || recovery.due.is_empty() {
        return Ok(got);
    }
    for key in &recovery.due {
        let path = format!("repos/{}/actions/runs/{key}/rerun-failed-jobs", p.repo);
        match call(p.api, "POST", &path, None, None).map_err(|e| p.noted(e)) {
            Ok(_) => eprintln!(
                "{}: ran {key} again, cancelled with nothing run through since",
                p.repo
            ),
            Err(e) if e.unanswered => return Err(e),
            Err(e) => {
                if e.why.contains("403") || e.why.contains("not accessible") {
                    recovery.refused.insert(key.clone());
                }
                eprintln!("{}: run {key} not run again: {e}", p.repo);
            }
        }
    }
    recovery.lookup = recovery.runs.keys().cloned().collect();
    read(p, &mut recovery)?;
    Ok(classify(
        p,
        ask,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &timed_out,
        &mut recovery,
    ))
}

fn read(p: &mut Pass, recovery: &mut Recovery) -> Result<(), Error> {
    for key in std::mem::take(&mut recovery.lookup) {
        let got = p.get(&format!("repos/{}/actions/runs/{key}", p.repo))?;
        let run =
            serde_json::from_value(got).map_err(|e| Error::said(format!("run {key}: {e}")))?;
        recovery.runs.insert(key, run);
    }
    Ok(())
}

fn classify(
    p: &Pass,
    ask: &Ask,
    contexts: &[Value],
    suites: &[Value],
    runs: &ci_runs::Runs,
    timed_out: &BTreeSet<u64>,
    recovery: &mut Recovery,
) -> Verdict {
    recovery.due.clear();
    let proofs = if runs.docs { &[][..] } else { &ask.proofs };
    let spelled: BTreeMap<_, _> = proofs
        .iter()
        .map(|s| (s.to_lowercase(), s.as_str()))
        .collect();
    let mut red = Vec::new();
    let base = text(&ask.pr["baseRefName"]).unwrap_or("");
    let mut pending: Vec<_> = runs
        .unrun
        .iter()
        .map(|w| format!("{w} for {base}"))
        .collect();
    let mut seen = runs.unrun.len();
    let mut broken = BTreeSet::new();
    let mut going = BTreeSet::new();
    let mut cancelled_jobs = BTreeSet::new();
    for s in suites {
        if s["conclusion"] == "STARTUP_FAILURE" {
            seen += 1;
            red.push(format!(
                "{} (failed to start)",
                text(&s["workflowRun"]["workflow"]["name"]).unwrap_or("a workflow")
            ));
        }
    }
    for c in contexts {
        if c["__typename"] == "CheckRun" {
            if runs.stale.contains(text(&suite(c)["id"]).unwrap_or("")) {
                continue;
            }
            let after = if cancelled(c) {
                ci_runs::after(c, contexts, &runs.stale)
            } else {
                After::None
            };
            if after == After::Finished {
                continue;
            }
            seen += 1;
            let name = text(&c["name"]).unwrap_or("");
            if c["status"] != "COMPLETED" || suite(c)["status"] != "COMPLETED" {
                going.insert(run_id(c));
            }
            if c["status"] != "COMPLETED" || after == After::Going {
                pending.push(name.into());
            } else if cancelled(c)
                && c["databaseId"]
                    .as_u64()
                    .is_some_and(|id| timed_out.contains(&id))
            {
                red.push(format!("{name} (timed out)"));
                broken.insert(run_id(c));
            } else if cancelled(c) {
                let key = if ask.recover { rerun_key(c) } else { None };
                let info = key.as_ref().and_then(|k| recovery.runs.get(k));
                match key {
                    None => pending.push(format!("{name} (cancelled, to be run again)")),
                    Some(k) if recovery.refused.contains(&k) => pending.push(format!("{name} (cancelled; the App may not run it again, which needs Actions write)")),
                    Some(k) => match info {
                        None => { recovery.lookup.insert(k); pending.push(format!("{name} (cancelled, to be run again)")); }
                        Some(r) if r.status != "completed" || r.conclusion.as_deref() != Some("cancelled") => pending.push(format!("{name} (cancelled, run again)")),
                        Some(r) if r.run_attempt >= 2 => red.push(format!("{name} (cancelled again after a rerun)")),
                        Some(_) => { cancelled_jobs.insert(Some(k)); pending.push(format!("{name} (cancelled, to be run again)")); }
                    }
                }
            } else if !["SUCCESS", "NEUTRAL", "SKIPPED"]
                .iter()
                .any(|s| c["conclusion"] == *s)
            {
                red.push(name.into());
                broken.insert(run_id(c));
            } else if workflow(c).is_some() && suite(c)["status"] != "COMPLETED" {
                pending.push(format!("{} (still running)", workflow_name(c)));
            }
        } else {
            let mut name = text(&c["context"]).unwrap_or("").to_owned();
            if name == p.cfg.settle.context {
                continue;
            }
            if name == p.cfg.settle.train_context {
                if c["state"] != "FAILURE" || !mine(p, &c["creator"]) {
                    continue;
                }
                name = format!(
                    "{name} ({})",
                    text(&c["description"]).unwrap_or("red alone")
                );
            }
            seen += 1;
            name = spelled
                .get(&name.to_lowercase())
                .map_or(name.clone(), |s| s.to_string());
            if ["PENDING", "EXPECTED"].iter().any(|s| c["state"] == *s) {
                pending.push(name);
            } else if c["state"] != "SUCCESS" {
                red.push(name);
            }
        }
    }
    recovery.due.extend(
        cancelled_jobs
            .difference(&broken)
            .filter(|k| !going.contains(*k))
            .flatten()
            .cloned(),
    );
    let named: BTreeSet<_> = contexts
        .iter()
        .filter_map(|c| {
            text(&c["name"]).or_else(|| {
                text(&c["context"]).map(|n| spelled.get(&n.to_lowercase()).copied().unwrap_or(n))
            })
        })
        .collect();
    for name in proofs {
        if !named.contains(name.as_str()) {
            seen += 1;
            pending.push(name.clone());
        }
    }
    let state = if seen == 0 {
        "none"
    } else if !red.is_empty() {
        "red"
    } else if !pending.is_empty() {
        "pending"
    } else {
        "green"
    };
    let mut names = if red.is_empty() { pending } else { red };
    let mut unique = BTreeSet::new();
    names.retain(|n| unique.insert(n.clone()));
    Verdict {
        state,
        names,
        docs: runs.docs,
    }
}

fn mine(p: &Pass, author: &Value) -> bool {
    author["__typename"] == "Bot" && author["login"] == p.cfg.app.slug
}

#[cfg(test)]
#[path = "ci_tests.rs"]
mod tests;
