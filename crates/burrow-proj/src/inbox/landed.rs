//! When a task list's child is done: a merged PR is tied to it
//! (docs/task-planning.md). Tied is a closing reference from any repo, a PR
//! the child's own job awaited before it said `proj done`, or one an author
//! recorded with `proj plan done`. The host keeps the last two in the
//! child's plan folder, and only the host writes them: `awaited`, every PR
//! or issue the job awaited, once the child is closed and the pass has
//! accepted the job's completion ([`declared_done`] and the issue as it was
//! handed it); `landed`, from `proj plan done` outside any job, under the
//! inbox's pass lock. An awaited one counts once it has merged, whenever
//! that is, and a reopened child drops what its earlier job awaited.
use super::{List, Pass, key};
use crate::{Fail, job::Job, read, swap};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

/// The child's landed PR, as an author recorded it.
const LANDED: &str = "landed";

/// What the child's finished job awaited, newest first, kept once the child
/// closed with that job's completion accepted.
const AWAITED: &str = "awaited";

/// How long a closed child whose job awaited a PR that hasn't merged waits
/// before its repo is looked at again: a merge elsewhere sends no event here.
const AWAITED_LOOK: i64 = 3600;

/// In a parent's plan folder: a notice that its list waits may be open, so
/// it's closed (and that confirmed) once the list stops waiting.
const NOTICE: &str = "notice";

/// Whether an issue may close with its finished job ([`Pass::child_can_close`]).
#[derive(Debug, PartialEq)]
pub(crate) enum Closing {
    No,
    Yes,
    /// Yes, by this merged PR the job awaited: what the job awaited is kept
    /// as the child's once the close has gone through.
    Awaited(String),
}

impl Closing {
    pub(crate) fn closes(&self) -> bool {
        *self != Closing::No
    }
}

/// Whether the job's latest turn is its own `proj done`: run to its end,
/// not stopped, not the synthetic turn a one-way question's timeout writes,
/// and no owner's answer owed. Both the open and closed child's paths ask
/// this, under the job's lock where they act on it.
pub(in crate::inbox) fn declared_done(job: &Job) -> bool {
    let Some(turn) = job.turns().filter(|t| !t.is_empty()) else {
        return false;
    };
    job.state() == "done"
        && read(&job.turn(&turn, "end")).as_deref() == Some("done")
        && !job.turn(&turn, "stopped").exists()
        && !job.turn(&turn, "timeout").exists()
        && !crate::ask::host_path(job, "question-needs-owner").exists()
}

/// Every PR or issue the job's turns awaited, newest first, as `OWNER/REPO#N`.
fn awaited_targets(job: &Job) -> Vec<String> {
    let turns: u64 = job.turns().and_then(|t| t.parse().ok()).unwrap_or(0);
    let mut seen = std::collections::BTreeSet::new();
    (1..=turns)
        .rev()
        .flat_map(|turn| {
            let text = read(&job.turn(&turn.to_string(), "github-awaits")).unwrap_or_default();
            text.lines()
                .rev()
                .filter_map(|l| reference(l.split('\t').next().unwrap_or_default()))
                .map(|(r, n)| format!("{r}#{n}"))
                .collect::<Vec<_>>()
        })
        .filter(|t| seen.insert(t.to_lowercase()))
        .collect()
}

/// `OWNER/REPO#N`, as `proj await` and `proj plan done` take one.
fn reference(s: &str) -> Option<(String, u64)> {
    let (repo, n) = s.trim().split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    (part(owner) && part(name))
        .then(|| {
            n.parse()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| (repo.to_owned(), n))
        })
        .flatten()
}

/// `https://github.com/OWNER/REPO/pull/N` as `OWNER/REPO#N`.
fn pull_url(s: &str) -> Option<(String, u64)> {
    let path = s.trim().strip_prefix("https://github.com/")?;
    let (repo, n) = path.trim_end_matches('/').rsplit_once("/pull/")?;
    reference(&format!("{repo}#{n}"))
}

impl Pass<'_> {
    /// The merged PR tied to child N: Some(Some(url)) when there is one,
    /// Some(None) when there's none, None when GitHub couldn't be asked.
    /// What a finished job awaited counts only for a `closed` child.
    pub(super) fn landed_child(
        &mut self,
        repo: &str,
        n: u64,
        closed: bool,
    ) -> Option<Option<String>> {
        if let Some(pr) = self.recorded_landing(repo, n) {
            return Some(Some(pr));
        }
        if closed {
            let kept = self.plan_dir(repo, &n.to_string()).join(AWAITED);
            let targets: Vec<String> = read(&kept)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if let Some(pr) = self.first_merged(&targets)? {
                return Some(Some(pr));
            }
            if !targets.is_empty() {
                self.book(repo, crate::now() + AWAITED_LOOK);
            }
        }
        // A child split again is done when its own list is.
        if let Ok(bytes) = fs::read(self.plan_dir(repo, &n.to_string()).join("tasks.json"))
            && let Ok(list) = serde_json::from_slice::<List>(&bytes)
            && !list.children.is_empty()
        {
            return Some(
                list.next()
                    .is_none()
                    .then(|| format!("https://github.com/{repo}/issues/{n}")),
            );
        }
        // A closing reference, from whichever repo.
        let (owner, name) = repo.split_once('/')?;
        let query = "query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){issue(number:$n){closedByPullRequestsReferences(first:100,includeClosedPrs:true){nodes{merged url}}}}}";
        let got = crate::github::query_for(
            self.cfg,
            repo,
            query,
            json!({"owner":owner,"name":name,"n":n}),
        )?;
        Some(
            got["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"]
                .as_array()?
                .iter()
                .find(|pr| pr["merged"] == true)
                .and_then(|pr| pr["url"].as_str())
                .map(str::to_owned),
        )
    }

    pub(super) fn recorded_landing(&self, repo: &str, n: u64) -> Option<String> {
        read(&self.plan_dir(repo, &n.to_string()).join(LANDED)).filter(|pr| !pr.is_empty())
    }

    fn record_landing(&self, repo: &str, n: u64, pr: &str) -> bool {
        let d = self.plan_dir(repo, &n.to_string());
        fs::create_dir_all(&d).is_ok() && swap(&d.join(LANDED), &format!("{pr}\n")).is_ok()
    }

    /// Keep what child N's job awaited, once N is closed with that job's
    /// completion accepted.
    pub(in crate::inbox) fn keep_awaited(&self, repo: &str, n: u64, job: &Job) -> bool {
        let targets = awaited_targets(job);
        if targets.is_empty() {
            return true;
        }
        let d = self.plan_dir(repo, &n.to_string());
        fs::create_dir_all(&d).is_ok()
            && swap(&d.join(AWAITED), &format!("{}\n", targets.join("\n"))).is_ok()
    }

    /// A reopened child: its earlier job's awaits are no evidence for it now.
    pub(super) fn drop_awaited(&self, repo: &str, n: u64) -> bool {
        match fs::remove_file(self.plan_dir(repo, &n.to_string()).join(AWAITED)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => false,
            _ => true,
        }
    }

    /// The first of these that's a merged PR, as its URL.
    fn first_merged(&self, targets: &[String]) -> Option<Option<String>> {
        for target in targets {
            let Some((at, m)) = reference(target) else {
                continue;
            };
            let issue = crate::github::api(self.cfg, &format!("repos/{at}/issues/{m}"))?;
            if let Some(pr) = merged(&issue) {
                return Some(Some(pr));
            }
        }
        Some(None)
    }

    /// Whether N is a child on any of the repo's task lists.
    pub(in crate::inbox) fn is_child(&self, repo: &str, n: u64) -> bool {
        self.plan_dirs(repo).iter().any(|d| {
            fs::read(d.join("tasks.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<List>(&bytes).ok())
                .is_some_and(|list| list.children.iter().any(|c| c.number == Some(n)))
        })
    }

    /// The newest PR any of the job's turns awaited that has merged, in
    /// whichever repo. Some(None) when none has, None when GitHub couldn't
    /// be asked. Reads only.
    pub(super) fn awaited_landing(&self, job: &Job) -> Option<Option<String>> {
        self.first_merged(&awaited_targets(job))
    }

    /// A child's issue closed, by hand say, before the pass that would have
    /// closed it with its finished job: what that job awaited is kept all
    /// the same, before the job's record goes, if its completion is one the
    /// pass would have accepted ([`declared_done`], every answer posted,
    /// handed the issue as it stands and no reply since). Kept whether or
    /// not a PR of it has merged yet: the sequencer counts one once it has.
    /// A hand close with no such job keeps nothing. False when GitHub
    /// couldn't be asked: the record is kept for the next pass.
    pub(in crate::inbox) fn keep_landing(
        &self,
        repo: &str,
        n: &str,
        job: &Job,
        d: &Path,
        issue: &Value,
    ) -> bool {
        let Ok(child) = n.parse::<u64>() else {
            return true;
        };
        if !self.inbox.planning || !self.is_child(repo, child) {
            return true;
        }
        let turns = job.turns().unwrap_or_default();
        let number = |path: &Path| read(path).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
        if !declared_done(job) || number(&d.join("answered")) < number(&job.path("turns")) {
            return true;
        }
        let Some(written) = self.written(repo, issue) else {
            return false;
        };
        let seen = number(&d.join("seen"));
        let after = |c: &Value| c["id"].as_i64().is_some_and(|i| i > seen);
        let had: Vec<Value> = written
            .comments
            .iter()
            .filter(|c| !after(c))
            .cloned()
            .collect();
        if !Self::handed(d, &self.issue_text(repo, &written.issue, &had))
            || written.comments.iter().any(|c| after(c) && self.heard(c))
        {
            return true;
        }
        let Ok(_held) = job.lock() else {
            return false;
        };
        job.turns().as_deref() != Some(turns.as_str())
            || !declared_done(job)
            || self.keep_awaited(repo, child, job)
    }

    /// Notices owed for lists the open-parent pass doesn't reach: a parent
    /// closed (finished, by hand) or no longer listed, or a list replanned
    /// to nothing or done. Each is closed, and retried until confirmed.
    pub(super) fn settle_notices(&mut self, repo: &str, open: &[Value]) {
        for d in self.plan_dirs(repo) {
            if !d.join(NOTICE).exists() {
                continue;
            }
            let Some(parent) = d
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.rsplit_once('-'))
                .map(|(_, n)| n.to_owned())
            else {
                continue;
            };
            let listed = open
                .iter()
                .any(|i| super::raw(&i["number"]) == parent && self.eligible(repo, i));
            let waiting = fs::read(d.join("tasks.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<List>(&b).ok())
                .is_some_and(|list| !list.tasks.is_empty() && list.next().is_some());
            if !listed || !waiting {
                self.reconcile_notice(repo, &parent, None);
            }
        }
    }

    /// The child a notice that the list waits may still be open about.
    pub(super) fn notice_owed(&self, repo: &str, parent: &str) -> Option<u64> {
        read(&self.plan_dir(repo, parent).join(NOTICE)).and_then(|n| n.parse().ok())
    }

    /// Tell the owner, through the notifier, while the list waits on a
    /// child closed with no landed PR, and clear that once it doesn't. Both
    /// are retried every pass until they hold: `ensure` is safe to repeat,
    /// and a close counts only once `pending` says the notice is gone.
    pub(super) fn reconcile_notice(&mut self, repo: &str, parent: &str, waiting_on: Option<u64>) {
        let Some(notify) = &self.cfg.notify else {
            return;
        };
        let owed = self.plan_dir(repo, parent).join(NOTICE);
        let id = format!("proj-plan-{}-{parent}", key(repo));
        let run = |args: &[&str]| Command::new(notify).args(args).status().ok();
        match waiting_on {
            Some(child) => {
                // Kept before it's asked for, so a crash still owes a close.
                if swap(&owed, &format!("{child}\n")).is_err() {
                    eprintln!("proj-inbox: couldn't keep {}", owed.display());
                    return;
                }
                let said = run(&[
                    "ensure",
                    &id,
                    &format!("{repo}#{parent}'s task list waits on #{child}"),
                    &format!(
                        "{repo}#{child} closed without a merged PR tied to it, so the task list on {repo}#{parent} waits and its next child doesn't start. If its work landed, an author records the PR with `proj plan done {repo}#{child} PR-URL`; otherwise reopen it."
                    ),
                ]);
                // pending exits 0 once the notifier holds the notice.
                if !said.is_some_and(|s| s.success())
                    || !run(&["pending", &id]).is_some_and(|s| s.code() == Some(0))
                {
                    eprintln!("proj-inbox: couldn't tell the owner {repo}#{parent} waits");
                    self.retry(repo);
                }
            }
            None if owed.exists() => {
                run(&["close", &id, "The task list moved on."]);
                // pending exits 1 once the notice is known closed.
                if run(&["pending", &id]).is_some_and(|s| s.code() == Some(1)) {
                    let _ = fs::remove_file(&owed);
                } else {
                    self.retry(repo);
                }
            }
            None => {}
        }
    }

    /// `proj plan done`'s work, under the inbox's pass lock: `caller` is the
    /// GitHub account it runs as. Answers the repo as its clone spells it.
    pub(crate) fn plan_done(
        &self,
        child: &str,
        pr: &str,
        caller: Option<u64>,
    ) -> Result<String, Fail> {
        let usage = || Fail::new(2, PLAN_USAGE);
        let (asked, n) = reference(child).ok_or_else(usage)?;
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
        let (at, m) = pull_url(pr).ok_or_else(usage)?;
        if !self.allowed(caller) {
            return Err(Fail::new(
                1,
                "proj plan done: only one of the inbox's authors may record a child's landed PR, and the GitHub account this runs as isn't one",
            ));
        }
        if !self.is_child(&repo, n) {
            return Err(Fail::new(
                1,
                format!("proj plan done: {repo}#{n} isn't a child on any task list"),
            ));
        }
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
        if !self.record_landing(&repo, n, &url) {
            return Err(Fail::new(
                1,
                format!("proj plan done: couldn't record {repo}#{n}'s landed PR"),
            ));
        }
        Ok(repo)
    }
}

/// A merged PR's URL, from its issue as REST gives it.
fn merged(issue: &Value) -> Option<String> {
    let pr = issue.get("pull_request")?;
    pr["merged_at"].as_str().filter(|t| !t.is_empty())?;
    pr["html_url"]
        .as_str()
        .or_else(|| issue["html_url"].as_str())
        .map(str::to_owned)
}

pub(crate) const PLAN_USAGE: &str = "usage: proj plan done OWNER/REPO#N PR-URL (an author records the merged PR that landed a task list's child)";

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn references_and_pull_urls() {
        assert_eq!(reference("sam/box#8"), Some(("sam/box".into(), 8)));
        assert_eq!(reference("sam/box#0"), None);
        assert_eq!(reference("sam#8"), None);
        assert_eq!(reference("sam/b x#8"), None);
        assert_eq!(
            pull_url("https://github.com/sam/box/pull/10"),
            Some(("sam/box".into(), 10))
        );
        assert_eq!(pull_url("https://github.com/sam/box/issues/10"), None);
        assert_eq!(pull_url("https://example.com/sam/box/pull/10"), None);
    }
}
