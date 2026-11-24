//! A head whose red check a fix job found the cause of outside the PR (the
//! machine's, another repo's), named with the issue or PR that tracks it
//! (claims.rs's `Tracker`): the steward hands the head to no other job, and
//! asks this while it waits, reading the tracker as its own owner's App
//! first, so one in another owner's private repo is seen. Open, it waits
//! on. Closed, each red check's
//! run that last started before it closed is run again whole, through the
//! one rerun door (ci::rerun), so it reads its workflows afresh; one that
//! started since (the job's own rerun, or this one's) is a verdict after
//! the fix, the head's own, and isn't run again here: so a run goes again
//! once for each closing, whoever asked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ci::{self, Rerun};
use crate::pass::Pass;
use burrow_github::Error;

/// What the steward asks, in turn: first with no `ci`, the tracker read
/// (as the App's installation on the tracker's owner, which can see it);
/// then, once that says it closed, with `closed` and the head's `ci`, its
/// red runs seen to (as the PR's owner's).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// What tracks the cause, as `OWNER/REPO#N`.
    pub tracker: String,
    /// When it closed, in Unix seconds, as the read said.
    #[serde(default)]
    pub closed: Option<f64>,
    /// The head's CI, as the verdict is asked about it.
    #[serde(default)]
    pub ci: Option<ci::Ask>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Tracked {
    /// The tracker is open: the head waits on it.
    pub open: bool,
    /// When it closed, in Unix seconds, once it has.
    pub closed: Option<f64>,
    /// GitHub has no such issue or PR (it says so: a 404 or 410), or the
    /// name isn't one: nothing to wait on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
    /// Runs asked to run again now.
    pub reran: Vec<String>,
    /// Runs going already (run again, by anyone): their verdict is to come.
    pub going: Vec<String>,
    /// Runs GitHub wouldn't run again, and what it said.
    pub refused: BTreeMap<String, String>,
    /// Runs that ran since it closed: red on their own.
    pub since: Vec<String>,
}

pub fn tracked(p: &mut Pass, ask: &Ask) -> Result<Tracked, Error> {
    match (&ask.ci, ask.closed) {
        (None, _) => read(p, &ask.tracker),
        (Some(ci), Some(closed)) => again(p, ci, closed),
        (Some(_), None) => Err(Error::said(
            "a head's red runs go again only once its tracker closed",
        )),
    }
}

/// The tracker: open, closed and when, or none GitHub has. Any other no
/// (a token that won't do) fails the word, as GitHub not answering does:
/// only GitHub saying there's none lets the head go to a job.
fn read(p: &mut Pass, tracker: &str) -> Result<Tracked, Error> {
    let mut out = Tracked::default();
    let Some((repo, n)) = tracker
        .split_once('#')
        .filter(|_| crate::answer::tracker(tracker))
    else {
        out.unknown = Some(format!("{tracker:?} isn't OWNER/REPO#N"));
        return Ok(out);
    };
    let issue = match p.get(&format!("repos/{repo}/issues/{n}")) {
        Ok(v) => v,
        Err(e)
            if !e.unanswered
                && (e.why.contains(": HTTP 404:") || e.why.contains(": HTTP 410:")) =>
        {
            out.unknown = Some(e.why);
            return Ok(out);
        }
        Err(e) => return Err(e),
    };
    if issue["state"] != "closed" {
        out.open = true;
        return Ok(out);
    }
    out.closed = Some(
        issue["closed_at"]
            .as_str()
            .and_then(|t| crate::ts(t).ok())
            .ok_or_else(|| Error::said(format!("{tracker}: closed, and no time it closed")))?,
    );
    Ok(out)
}

/// Each red run on the head that last started before `closed`, run again.
fn again(p: &mut Pass, ci: &ci::Ask, closed: f64) -> Result<Tracked, Error> {
    let mut out = Tracked {
        closed: Some(closed),
        ..Tracked::default()
    };
    for run in ci::red_runs(p, ci) {
        let got = p.get(&format!("repos/{}/actions/runs/{run}", p.repo))?;
        let started = got["run_started_at"]
            .as_str()
            .and_then(|t| crate::ts(t).ok());
        if got["status"] != "completed" {
            out.going.push(run);
        } else if started.is_none_or(|s| s >= closed) {
            // No start to go by is none before it: never run again here.
            out.since.push(run);
        } else {
            match ci::rerun(p, &run, Rerun::Whole)? {
                Ok(()) => out.reran.push(run),
                Err(said) => {
                    out.refused.insert(run, said);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "tracked_tests.rs"]
mod tests;
