//! `proj plan done`'s work (plan_door.rs takes the command): an author, on
//! the host and never from a job, records that a task list's child is done
//! when no pass could tell (landed.rs).
//! Either the merged PR that landed it some other way (`landed`), or, for
//! a closed child whose deliverable was a record or a judgment rather than
//! a PR, why none was needed (`no-pr`), bound to the closure it was given
//! on as `awaited` is: a reopened, edited or reclosed child doesn't keep it.
use super::super::super::Pass;
use super::{LANDED, NO_PR, closure, merged};
use crate::planning::reference;
use crate::{Fail, swap};
use serde_json::json;
use std::fs;

pub(crate) const PLAN_USAGE: &str = "usage: proj plan done OWNER/REPO#N PR-URL (an author records the merged PR that landed a task list's child)\n       proj plan done OWNER/REPO#N --no-pr REASON (an author records why a closed child needed no PR: its record was the deliverable)\n       proj plan end OWNER/REPO#N (an author ends a parent's task list, leaving the parent open)";

/// `https://github.com/OWNER/REPO/pull/N` as `OWNER/REPO#N`.
fn pull_url(s: &str) -> Option<(String, u64)> {
    let path = s.trim().strip_prefix("https://github.com/")?;
    let (repo, n) = path.trim_end_matches('/').rsplit_once("/pull/")?;
    reference(&format!("{repo}#{n}"))
}

impl Pass<'_> {
    /// What a `proj plan` door is asked about, `OWNER/REPO#N`: the repo as
    /// its clone spells it (the plans are kept under that), and N. Refused
    /// for a repo that isn't one of the machine's projects, and for a
    /// caller, the GitHub account it runs as, that isn't one of the inbox's
    /// authors.
    pub(in super::super) fn plan_door(
        &self,
        door: &str,
        issue: &str,
        caller: Option<u64>,
    ) -> Result<(String, u64), Fail> {
        let (asked, n) = reference(issue).ok_or_else(|| Fail::new(2, PLAN_USAGE))?;
        let repo = self
            .project
            .keys()
            .find(|r| r.eq_ignore_ascii_case(&asked))
            .cloned()
            .ok_or_else(|| {
                Fail::new(
                    1,
                    format!("proj plan {door}: {asked} isn't one of this machine's projects"),
                )
            })?;
        if !self.allowed(caller) {
            return Err(Fail::new(
                1,
                format!(
                    "proj plan {door}: only one of the inbox's authors may change a task list, and the GitHub account this runs as isn't one"
                ),
            ));
        }
        Ok((repo, n))
    }

    /// What both forms of `proj plan done` ask first: the door's checks,
    /// and that `child` is a child on one of the repo's task lists.
    fn plan_child(&self, child: &str, caller: Option<u64>) -> Result<(String, u64), Fail> {
        let (repo, n) = self.plan_door("done", child, caller)?;
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
