//! A head whose red check a fix job found the cause of outside the PR (the
//! machine's, another repo's), named with the issue or PR that tracks it
//! (claims.rs's `Tracker`): the steward hands the head to no other job, and
//! asks this while it waits. Open, it waits on. Closed, each red check's
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// What tracks the cause, as `OWNER/REPO#N`.
    pub tracker: String,
    /// The head's CI, as the verdict is asked about it.
    pub ci: ci::Ask,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Tracked {
    /// The tracker is open: the head waits on it.
    pub open: bool,
    /// GitHub has no such issue or PR to read, in its words: nothing to
    /// wait on.
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
    let mut out = Tracked::default();
    let Some((repo, n)) = ask
        .tracker
        .split_once('#')
        .filter(|_| crate::answer::tracker(&ask.tracker))
    else {
        out.unknown = Some(format!("{:?} isn't OWNER/REPO#N", ask.tracker));
        return Ok(out);
    };
    let issue = match p.get(&format!("repos/{repo}/issues/{n}")) {
        Ok(v) => v,
        Err(e) if e.unanswered => return Err(e),
        Err(e) => {
            out.unknown = Some(e.why);
            return Ok(out);
        }
    };
    if issue["state"] != "closed" {
        out.open = true;
        return Ok(out);
    }
    let closed = issue["closed_at"]
        .as_str()
        .and_then(|t| crate::ts(t).ok())
        .ok_or_else(|| Error::said(format!("{}: closed, and no time it closed", ask.tracker)))?;
    for run in ci::red_runs(p, &ask.ci) {
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
