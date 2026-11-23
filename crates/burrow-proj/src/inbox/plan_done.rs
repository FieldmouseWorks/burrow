//! `proj plan done`: an author, on the host and never from a job, records
//! that a task list's child is done when no pass could tell (landed.rs).
//! Either the merged PR that landed it some other way (`landed`), or, for
//! a closed child whose deliverable was a record or a judgment rather than
//! a PR, why none was needed (`no-pr`), bound to the closure it was given
//! on as `awaited` is: a reopened, edited or reclosed child doesn't keep it.
use super::super::super::{FORGET_WAIT, Held, Pass, io_fail, projects, resumption};
use super::{LANDED, NO_PR, closure, merged};
use crate::config::Config;
use crate::planning::reference;
use crate::{Fail, swap};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;

pub(crate) const PLAN_USAGE: &str = "usage: proj plan done OWNER/REPO#N PR-URL (an author records the merged PR that landed a task list's child)\n       proj plan done OWNER/REPO#N --no-pr REASON (an author records why a closed child needed no PR: its record was the deliverable)";

/// What the author records about the child.
enum Done<'a> {
    /// The merged PR that landed it, as its URL.
    Pr(&'a str),
    /// Why it needed none, one line.
    NoPr(String),
}

/// `proj plan done OWNER/REPO#N PR-URL` or `... --no-pr REASON`: one of
/// the inbox's authors, on the host and never from a job, records that a
/// task list's child is done, and the list moves on at its repo's next
/// pass. The author is the GitHub account the caller's own gh answers as.
pub fn plan(cfg: &Config, args: &[String]) -> Result<(), Fail> {
    let usage = || Fail::new(2, PLAN_USAGE);
    let (child, rest) = match args {
        [done, child, rest @ ..] if done == "done" => (child, rest),
        _ => return Err(usage()),
    };
    let what = match rest {
        [pr] if pr != "--no-pr" => Done::Pr(pr),
        [flag, reason @ ..] if flag == "--no-pr" => {
            let reason = reason.join(" ");
            let reason = reason.trim();
            if reason.is_empty() || reason.contains('\n') {
                return Err(usage());
            }
            Done::NoPr(reason.to_owned())
        }
        _ => return Err(usage()),
    };
    if std::env::var_os("PROJ_JOB").is_some_and(|id| !id.is_empty()) {
        return Err(Fail::new(
            2,
            "proj plan done is the host's, for an author: a job never records a child done",
        ));
    }
    let inbox = cfg
        .inbox
        .as_ref()
        .filter(|i| i.planning)
        .ok_or_else(|| Fail::new(1, "proj plan done: inbox planning isn't on here"))?;
    let caller = crate::github::reported(burrow_github::call_repeating(
        &mut burrow_github::Gh::default(),
        "GET",
        "user",
        None,
    ))
    .and_then(|r| r.json().ok())
    .and_then(|v| v["id"].as_u64());
    // Under the pass lock, as passes and `forget` are: the child's place on
    // its list and the record land together, with no pass writing beside it.
    let lock = PathBuf::from(format!("{}.lock", inbox.wake.display()));
    let held = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&lock)
        .map_err(|e| io_fail(&lock, e))?;
    let Some(held) = Held::lock_within(held, FORGET_WAIT).map_err(|e| io_fail(&lock, e))? else {
        return Err(Fail::new(
            1,
            "proj plan done: an inbox pass is still going after ten minutes; nothing recorded",
        ));
    };
    let permit = resumption::Permit::pass(held);
    let p = Pass {
        cfg,
        inbox,
        project: projects(cfg),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
        observe: false,
        scope: None,
        drain_at: None,
        skipped: Vec::new(),
    };
    let (repo, said) = match &what {
        Done::Pr(pr) => (
            p.plan_done(child, pr, caller)?,
            format!("Recorded {pr} as {child}'s landed PR"),
        ),
        Done::NoPr(reason) => (
            p.plan_done_without_pr(child, reason, caller)?,
            format!("Recorded {child} as done without a PR"),
        ),
    };
    drop(permit);
    crate::job::kick(cfg, [format!("proj-inbox/{repo}")]);
    println!("{said}; its task list moves on at the repo's next pass.");
    Ok(())
}

/// `https://github.com/OWNER/REPO/pull/N` as `OWNER/REPO#N`.
fn pull_url(s: &str) -> Option<(String, u64)> {
    let path = s.trim().strip_prefix("https://github.com/")?;
    let (repo, n) = path.trim_end_matches('/').rsplit_once("/pull/")?;
    reference(&format!("{repo}#{n}"))
}

impl Pass<'_> {
    /// What both forms ask first: `child` is a child on one of this
    /// machine's projects' task lists, and `caller`, the GitHub account
    /// this runs as, one of the inbox's authors. Answers the repo as its
    /// clone spells it, and the child's number.
    fn plan_child(&self, child: &str, caller: Option<u64>) -> Result<(String, u64), Fail> {
        let (asked, n) = reference(child).ok_or_else(|| Fail::new(2, PLAN_USAGE))?;
        // The plans are kept under the repo as its clone spells it.
        let repo = self
            .project
            .keys()
            .find(|r| r.eq_ignore_ascii_case(&asked))
            .cloned()
            .ok_or_else(|| {
                Fail::new(
                    1,
                    format!("proj plan done: {asked} isn't one of this machine's projects"),
                )
            })?;
        if !self.allowed(caller) {
            return Err(Fail::new(
                1,
                "proj plan done: only one of the inbox's authors may record a child done, and the GitHub account this runs as isn't one",
            ));
        }
        if !self.is_child(&repo, n) {
            return Err(Fail::new(
                1,
                format!("proj plan done: {repo}#{n} isn't a child on any task list"),
            ));
        }
        Ok((repo, n))
    }

    /// `proj plan done OWNER/REPO#N PR-URL`'s work, under the inbox's pass
    /// lock. Answers the repo as its clone spells it.
    pub(crate) fn plan_done(
        &self,
        child: &str,
        pr: &str,
        caller: Option<u64>,
    ) -> Result<String, Fail> {
        reference(child).ok_or_else(|| Fail::new(2, PLAN_USAGE))?;
        let (at, m) = pull_url(pr).ok_or_else(|| Fail::new(2, PLAN_USAGE))?;
        let (repo, n) = self.plan_child(child, caller)?;
        let issue =
            crate::github::api(self.cfg, &format!("repos/{at}/issues/{m}")).ok_or_else(|| {
                Fail::new(1, format!("proj plan done: couldn't read {pr} from GitHub"))
            })?;
        let url = merged(&issue).ok_or_else(|| {
            Fail::new(
                1,
                format!("proj plan done: {pr} hasn't merged, so nothing is recorded"),
            )
        })?;
        if !self.keep_record(&repo, n, LANDED, &format!("{url}\n")) {
            return Err(Fail::new(
                1,
                format!("proj plan done: couldn't record {repo}#{n}'s landed PR"),
            ));
        }
        Ok(repo)
    }

    /// `proj plan done OWNER/REPO#N --no-pr REASON`'s work, under the
    /// inbox's pass lock: the child must be closed, and what's kept is
    /// bound to that closure. Answers the repo as its clone spells it.
    pub(crate) fn plan_done_without_pr(
        &self,
        child: &str,
        reason: &str,
        caller: Option<u64>,
    ) -> Result<String, Fail> {
        let (repo, n) = self.plan_child(child, caller)?;
        let issue =
            crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}")).ok_or_else(|| {
                Fail::new(
                    1,
                    format!("proj plan done: couldn't read {repo}#{n} from GitHub"),
                )
            })?;
        if issue["state"] != "closed" || !issue["closed_at"].is_string() {
            return Err(Fail::new(
                1,
                format!(
                    "proj plan done: {repo}#{n} is still open; close it first, so a done without a PR is bound to that closure"
                ),
            ));
        }
        let kept = json!({"closure": closure(&issue), "reason": reason});
        if !self.keep_record(&repo, n, NO_PR, &kept.to_string()) {
            return Err(Fail::new(
                1,
                format!("proj plan done: couldn't record {repo}#{n} as done without a PR"),
            ));
        }
        Ok(repo)
    }

    /// Keep `text` as child N's `name` record, in its plan folder.
    fn keep_record(&self, repo: &str, n: u64, name: &str, text: &str) -> bool {
        let d = self.plan_dir(repo, &n.to_string());
        fs::create_dir_all(&d).is_ok() && swap(&d.join(name), text).is_ok()
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn pull_urls() {
        assert_eq!(
            pull_url("https://github.com/sam/box/pull/10"),
            Some(("sam/box".into(), 10))
        );
        assert_eq!(pull_url("https://github.com/sam/box/issues/10"), None);
        assert_eq!(pull_url("https://example.com/sam/box/pull/10"), None);
    }
}
