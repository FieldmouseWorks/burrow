//! The head's one CI verdict and cancellation recovery owner. The steward
//! hands over GraphQL nodes and gets one verdict; all remote reads and
//! reruns use its inherited App token. GitHub's run_attempt is the only
//! retry store, shared across PRs, process restarts and uncertain writes.
//! A run GitHub reruns for no one (event `dynamic`: a default setup's, as
//! CodeQL's) comes back on a new head instead: the PR's branch brought up
//! to date with its base, a merge its reviewers' looks carry over
//! (carry.rs), and the head it leaves is the store.

use std::collections::{BTreeMap, BTreeSet};

use burrow_github::{Error, call, pages};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ci_runs::{self, cancelled, job, suite, workflow_name};
use crate::pass::Pass;

// The shared workflows' contract tests use the CI owner's names and
// recognition functions, so a language or representation change cannot
// silently leave their assertions reading a replaced implementation.
pub use crate::ci_runs::{DOCS, LANE, gate as gate_job, workflow};

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
    suite_run_id(suite(c))
}

pub(crate) fn suite_run_id(s: &Value) -> Option<String> {
    let id = &s["workflowRun"]["databaseId"];
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

/// How a workflow run is run again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rerun {
    /// Only its failed and cancelled jobs, keeping its successful ones.
    /// GitHub reads a called (reusable) workflow from the first attempt's
    /// commit for these.
    Failed,
    /// Every job, with each called workflow read afresh from its ref.
    Whole,
}

impl Rerun {
    fn action(self) -> &'static str {
        match self {
            Rerun::Failed => "rerun-failed-jobs",
            Rerun::Whole => "rerun",
        }
    }
}

/// The one door that runs a workflow run again, for the verdict's own
/// recovery and any caller that names a run. It asks, whatever the run's
/// run_attempt: the once-per-run allowance is the caller's to read (as
/// `check` does). A dry pass writes nothing. Ok(Err) is GitHub's no, in
/// its words; an unanswered ask keeps its typed retry classification, as
/// GitHub's run_attempt is the only store of whether it went through.
pub fn rerun(p: &mut Pass, run: &str, how: Rerun) -> Result<Result<(), String>, Error> {
    let action = how.action();
    if p.dry {
        eprintln!("{}: would ask to run {run} again ({action})", p.repo);
        return Ok(Ok(()));
    }
    let path = format!("repos/{}/actions/runs/{run}/{action}", p.repo);
    match call(p.api, "POST", &path, None, None).map_err(|e| p.noted(e)) {
        Ok(_) => {
            eprintln!(
                "{}: asked to run {run} again ({action}), with no live job",
                p.repo
            );
            Ok(Ok(()))
        }
        Err(e) if e.unanswered => Err(e),
        Err(e) => {
            eprintln!("{}: run {run} not run again: {}", p.repo, e.why);
            Ok(Err(quoted(&e)))
        }
    }
}

/// Whether one failure annotation is GitHub's, saying the job's runner
/// went away rather than the step failing, as the API gave them:
/// - shut down under it (run 45213576258, both attempts): its process got
///   SIGTERM and the step wrote no verdict of its own. GitHub's "The
///   runner has received a shutdown signal" is in the job's log, not its
///   annotations.
/// - lost (job 137975045560, #1489's `train / negative`): "The hosted
///   runner lost communication with the server. Anything in your workflow
///   that terminates the runner process, ..."
fn runner_gone(message: &str) -> bool {
    message == "Process completed with exit code 143."
        || message.starts_with("The hosted runner lost communication with the server.")
}

/// Whether a failed check run's annotations say only that its runner went
/// away: at least one of GitHub's runner-gone messages, and no failure
/// annotation of the step's own.
fn runner_lost(annotations: &[Value]) -> bool {
    let mut failures = annotations
        .iter()
        .filter(|a| a["annotation_level"] == "failure")
        .peekable();
    failures.peek().is_some() && failures.all(|a| text(&a["message"]).is_some_and(runner_gone))
}

#[derive(Default)]
struct Recovery {
    runs: BTreeMap<String, Run>,
    /// What GitHub said, by run, when it wouldn't run one again.
    refused: BTreeMap<String, String>,
    lookup: BTreeSet<String>,
    // A never-started run needs the whole workflow, and so does a lost
    // runner's under a called workflow, so it runs that workflow's
    // current copy; cancellations need only failed jobs, preserving
    // successful independent jobs.
    due: BTreeMap<String, Rerun>,
    /// A dynamic run wants a new head: the branch brought up to date.
    fresh: bool,
    /// How the ask to bring it up to date went: taken, or GitHub's words.
    updated: Option<Result<(), String>>,
    /// Check runs GitHub stopped for running past their timeout.
    timed_out: BTreeSet<u64>,
    /// Failed check runs whose runner was lost (no verdict of their own),
    /// with the run_attempt of the job each one is: the attempt it lost
    /// its runner in, whatever the run has gone on to since.
    lost: BTreeMap<u64, u64>,
}

#[derive(Deserialize)]
struct Run {
    run_attempt: u64,
    status: String,
    conclusion: Option<String>,
    /// What started it. GitHub refuses every rerun of a `dynamic` one
    /// ("This workflow run cannot be retried"), whoever asks.
    #[serde(default)]
    event: String,
}

impl Run {
    fn dynamic(&self) -> bool {
        self.event == "dynamic"
    }
}

impl Recovery {
    /// Why a run waits to be run again, when it's more than that it will
    /// be: GitHub won't rerun it, or said no to the last ask, in its words.
    fn again(&self, key: &str) -> Option<String> {
        if self.runs.get(key).is_some_and(Run::dynamic) {
            return Some(match &self.updated {
                None => "GitHub reruns no dynamic run: its branch is to be updated".into(),
                Some(Ok(())) => "GitHub reruns no dynamic run: its branch is being updated".into(),
                Some(Err(said)) => {
                    format!("GitHub reruns no dynamic run; updating its branch said {said}")
                }
            });
        }
        self.refused
            .get(key)
            .map(|said| format!("not run again: {said}"))
    }
}

/// GitHub's own words in a write's error: its status and message, without
/// the method and path before them.
fn quoted(e: &Error) -> String {
    e.why
        .find("HTTP ")
        .map_or(e.why.as_str(), |at| &e.why[at..])
        .to_owned()
}

/// Read a complete verdict, recover each eligible workflow once, and read
/// its current attempt back. An unanswered read/write fails the step with
/// its typed retry classification intact; a refused write is retried on
/// the next pass, with what GitHub said quoted in the verdict.
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
    let mut recovery = Recovery::default();
    let effective = ci_runs::effective(contexts, &runs.stale, ci_runs::docs_lane(ask));
    for c in effective
        .iter()
        .filter(|c| cancelled(c) || (c["status"] == "COMPLETED" && c["conclusion"] == "FAILURE"))
    {
        if ask.remote
            && let Some(id) = c["databaseId"].as_u64().filter(|id| *id > 0)
        {
            let path = format!("repos/{}/check-runs/{id}/annotations?per_page=100", p.repo);
            let pages = pages(p.api, &path).map_err(|e| p.noted(e))?;
            let annotations: Vec<_> = pages.iter().flat_map(array).cloned().collect();
            if !cancelled(c) {
                if runner_lost(&annotations) {
                    // An Actions check run's id is its job's.
                    let job = p.get(&format!("repos/{}/actions/jobs/{id}", p.repo))?;
                    let attempt = job["run_attempt"].as_u64().unwrap_or(1);
                    recovery.lost.insert(id, attempt);
                }
            } else if annotations.iter().any(|a| {
                text(&a["message"])
                    .is_some_and(|m| m.contains("has exceeded the maximum execution time"))
            }) {
                recovery.timed_out.insert(id);
            }
        }
    }
    let got = classify(
        p,
        ask,
        &effective,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &mut recovery,
    );
    if !ask.recover || !ask.remote || recovery.lookup.is_empty() {
        return Ok(got);
    }
    read(p, &mut recovery)?;
    let got = classify(
        p,
        ask,
        &effective,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &mut recovery,
    );
    if p.dry || (recovery.due.is_empty() && !recovery.fresh) {
        return Ok(got);
    }
    for (key, how) in &recovery.due {
        if let Err(said) = rerun(p, key, *how)? {
            recovery.refused.insert(key.clone(), said);
        }
    }
    if recovery.fresh {
        recovery.updated = Some(update(p, ask)?);
    }
    recovery.lookup = recovery.runs.keys().cloned().collect();
    read(p, &mut recovery)?;
    Ok(classify(
        p,
        ask,
        &effective,
        contexts,
        array(&suites["nodes"]),
        &runs,
        &mut recovery,
    ))
}

/// The workflow runs of the check runs that are red on the head, as the
/// verdict reads them: each failed one that still speaks for it. A
/// cancelled one is the verdict's own to recover, and a status has no run.
pub(crate) fn red_runs(p: &mut Pass, ask: &Ask) -> BTreeSet<String> {
    let contexts =
        array(&ask.pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]);
    let runs = ci_runs::runs(p, ask, contexts);
    ci_runs::effective(contexts, &runs.stale, ci_runs::docs_lane(ask))
        .into_iter()
        .filter(|c| {
            c["__typename"] == "CheckRun"
                && c["status"] == "COMPLETED"
                && !cancelled(c)
                && !["SUCCESS", "NEUTRAL", "SKIPPED"]
                    .iter()
                    .any(|s| c["conclusion"] == *s)
        })
        .filter_map(run_id)
        .collect()
}

/// Ask GitHub to merge the base into the PR's branch, at the head the
/// verdict is of (so never onto a head pushed since): a new head, whose
/// workflows run afresh. Ok(Err) is GitHub's no, in its words, for the
/// verdict to quote; the next pass asks again.
fn update(p: &mut Pass, ask: &Ask) -> Result<Result<(), String>, Error> {
    let (Some(n), Some(head)) = (ask.pr["number"].as_u64(), text(&ask.pr["headRefOid"])) else {
        return Ok(Err("nothing: the PR's number or head wasn't given".into()));
    };
    let path = format!("repos/{}/pulls/{n}/update-branch", p.repo);
    let body = json!({"expected_head_sha": head});
    match call(p.api, "PUT", &path, Some(&body), None).map_err(|e| p.noted(e)) {
        Ok(_) => {
            eprintln!(
                "{}#{n}: a dynamic run was cancelled, which GitHub won't rerun: bringing it up to date",
                p.repo
            );
            Ok(Ok(()))
        }
        Err(e) if e.unanswered => Err(e),
        Err(e) => {
            eprintln!("{}#{n}: not brought up to date: {}", p.repo, e.why);
            Ok(Err(quoted(&e)))
        }
    }
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
    effective: &[&Value],
    contexts: &[Value],
    suites: &[Value],
    runs: &ci_runs::Runs,
    recovery: &mut Recovery,
) -> Verdict {
    recovery.due.clear();
    recovery.fresh = false;
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
    let mut lost_runs: BTreeMap<String, Rerun> = BTreeMap::new();
    for s in suites {
        if s["conclusion"] == "STARTUP_FAILURE" {
            seen += 1;
            red.push(format!(
                "{} (failed to start)",
                text(&s["workflowRun"]["workflow"]["name"]).unwrap_or("a workflow")
            ));
        }
    }
    for s in crate::ci_unstarted::suites(suites, &runs.stale, contexts, base, p.now) {
        seen += 1;
        let name = text(&s["workflowRun"]["workflow"]["name"]).unwrap_or("a workflow");
        let key = if ask.recover { suite_run_id(s) } else { None };
        match key.as_deref().and_then(|k| recovery.again(k)) {
            Some(why) => pending.push(format!("{name} (never started; {why})")),
            None => pending.push(format!("{name} (never started, to be run again)")),
        }
        if let Some(key) = key {
            match recovery.runs.get(&key) {
                None => {
                    recovery.lookup.insert(key);
                }
                Some(r) if r.run_attempt == 1 && r.dynamic() => recovery.fresh = true,
                Some(r) if r.run_attempt == 1 => {
                    recovery.due.insert(key, Rerun::Whole);
                }
                Some(_) => {} // Another observer, restart or lost response already consumed the allowance.
            }
        }
    }
    for c in effective.iter().copied() {
        if c["__typename"] == "CheckRun" {
            seen += 1;
            let name = text(&c["name"]).unwrap_or("");
            if c["status"] != "COMPLETED" || suite(c)["status"] != "COMPLETED" {
                going.insert(run_id(c));
            }
            // A replacement run is alive: wait for its verdict before asking
            // GitHub to start this workflow yet again.
            let replaced = cancelled(c)
                && workflow(c).is_some()
                && contexts.iter().any(|x| {
                    x["__typename"] == "CheckRun"
                        && workflow(x) == workflow(c)
                        && suite(x)["id"] != suite(c)["id"]
                        && suite(x)["status"] != "COMPLETED"
                });
            let timed = cancelled(c)
                && c["databaseId"]
                    .as_u64()
                    .is_some_and(|id| recovery.timed_out.contains(&id));
            if c["status"] != "COMPLETED" || (replaced && !timed) {
                pending.push(name.into());
            } else if cancelled(c)
                && c["databaseId"]
                    .as_u64()
                    .is_some_and(|id| recovery.timed_out.contains(&id))
            {
                red.push(format!("{name} (timed out)"));
                broken.insert(run_id(c));
            } else if cancelled(c) {
                let key = if ask.recover { rerun_key(c) } else { None };
                let info = key.as_ref().and_then(|k| recovery.runs.get(k));
                match key {
                    None => pending.push(format!("{name} (cancelled, to be run again)")),
                    Some(k) => match info {
                        None => {
                            recovery.lookup.insert(k);
                            pending.push(format!("{name} (cancelled, to be run again)"));
                        }
                        Some(r)
                            if r.status != "completed"
                                || r.conclusion.as_deref() != Some("cancelled") =>
                        {
                            pending.push(format!("{name} (cancelled, run again)"))
                        }
                        Some(r) if r.run_attempt >= 2 => {
                            red.push(format!("{name} (cancelled again after a rerun)"))
                        }
                        Some(_) => {
                            match recovery.again(&k) {
                                Some(why) => pending.push(format!("{name} (cancelled; {why})")),
                                None => {
                                    pending.push(format!("{name} (cancelled, to be run again)"))
                                }
                            }
                            cancelled_jobs.insert(Some(k));
                        }
                    },
                }
            } else if let Some(&tried) = c["databaseId"]
                .as_u64()
                .and_then(|id| recovery.lost.get(&id))
            {
                // The runner went away: no verdict of the job's own. One
                // rerun, on the same allowance as a cancellation's; this
                // job lost its runner on a rerun itself, it's the
                // machine's, and named so. Only the job's own attempt says
                // that: the run's may be a later one's, still unread.
                let key = if ask.recover { rerun_key(c) } else { None };
                let info = key.as_ref().and_then(|k| recovery.runs.get(k));
                match (key, info) {
                    _ if tried >= 2 => {
                        red.push(format!("{name} (the runner was lost again after a rerun)"));
                        broken.insert(run_id(c));
                    }
                    (None, _) => pending.push(format!("{name} (runner lost, to be run again)")),
                    (Some(k), None) => {
                        recovery.lookup.insert(k);
                        pending.push(format!("{name} (runner lost, to be run again)"));
                    }
                    (Some(_), Some(r)) if r.status != "completed" || r.run_attempt >= 2 => {
                        pending.push(format!("{name} (runner lost, run again)"))
                    }
                    (Some(k), Some(_)) => {
                        match recovery.again(&k) {
                            Some(why) => pending.push(format!("{name} (runner lost; {why})")),
                            None => pending.push(format!("{name} (runner lost, to be run again)")),
                        }
                        // A called workflow is run whole, so it is read
                        // afresh from its ref rather than the first
                        // attempt's commit.
                        let how = if job(c).0.is_empty() {
                            Rerun::Failed
                        } else {
                            Rerun::Whole
                        };
                        let at = lost_runs.entry(k).or_insert(how);
                        *at = (*at).max(how);
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
    for key in cancelled_jobs
        .difference(&broken)
        .filter(|k| !going.contains(*k))
        .flatten()
    {
        if recovery.runs.get(key).is_some_and(Run::dynamic) {
            recovery.fresh = true;
        } else {
            recovery.due.insert(key.clone(), Rerun::Failed);
        }
    }
    for (key, how) in lost_runs.iter().filter(|(k, _)| {
        !broken.contains(&Some((*k).clone())) && !going.contains(&Some((*k).clone()))
    }) {
        if recovery.runs.get(key).is_some_and(Run::dynamic) {
            recovery.fresh = true;
        } else {
            let at = recovery.due.entry(key.clone()).or_insert(*how);
            *at = (*at).max(*how);
        }
    }
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
