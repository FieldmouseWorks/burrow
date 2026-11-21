//! When a task list's child is done: a merged PR is tied to it
//! (docs/task-planning.md). Tied is a closing reference from any repo, a PR
//! the child's own job awaited before it said `proj done`, the one its
//! task names it to carry (`carries`, read from the parent's list), or one
//! an author recorded with `proj plan done`. The host keeps the second and
//! last in the child's plan folder, and only the host writes them: `awaited`, every PR
//! or issue the job awaited, once the child is closed and the pass has
//! accepted the job's completion ([`declared_done`] and the issue as it was
//! handed it); `landed`, from `proj plan done` outside any job, under the
//! inbox's pass lock. An awaited one counts once it has merged, whenever
//! that is, and a reopened child drops what its earlier job awaited.
//!
//! A child closed as continued elsewhere (an author's comment on it with a
//! line `Continued as OWNER/REPO#N`) hands its place to that issue: the list
//! waits on it, saying so, and is done when a merged PR closes it. One
//! naming the parent or a child of its own list hands nothing on, and the
//! owner's notice says that it doesn't count.
use super::{List, Pass, key};
use crate::github::api;
use crate::inbox::record::files;
use crate::planning::reference;
use crate::record::TurnFile;
use crate::{Fail, job::Job, read, swap};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

/// The child's landed PR, as an author recorded it.
const LANDED: &str = "landed";

/// What the child's finished job awaited, newest first, kept once the child
/// closed with that job's completion accepted.
const AWAITED: &str = "awaited";

/// How long before a repo is looked at again while a closed child's awaited
/// PR hasn't merged (a merge elsewhere sends no event here), or while a list
/// waits and its notice is ensured again.
const AWAITED_LOOK: i64 = 3600;

/// In a parent's plan folder: a notice that its list waits may be open, so
/// it's closed (and that confirmed) once the list stops waiting.
const NOTICE: &str = "notice";

/// How an author's comment on a closed child names the issue its work went
/// on in, at the start of a line: `Continued as OWNER/REPO#N` or `#N`.
const CONTINUED: &str = "Continued as ";

/// How many continuations are followed from one child before the chain is
/// taken as having none.
const HOPS: usize = 4;

/// Where a closed child's work went on, by its successor ([`CONTINUED`]).
#[derive(Debug, PartialEq)]
pub(super) enum Successor {
    /// None named, or every one named closed without a merged PR.
    None,
    /// This merged PR closed the successor.
    Landed(String),
    /// The successor, `OWNER/REPO#N`, is open.
    Open(String),
    /// What's named, `OWNER/REPO#N`, is the parent or a child of the same
    /// list: never a successor, and the notice says so.
    OnList(String),
    /// GitHub couldn't be asked.
    Unknown,
}

/// What a list's notice to the owner says holds it ([`Pass::reconcile_notice`]).
#[derive(Debug, PartialEq)]
pub(super) enum Stall {
    /// Its next child, closed with nothing landed; `named`, the issue on
    /// the same list its newest `Continued as` names, if that's why.
    Child { child: u64, named: Option<String> },
    /// Its parent changed after its split ([`super::place`]).
    Paused,
}

/// What [`NOTICE`] keeps while the notice is about the parent's pause.
const PAUSED: &str = "paused";

/// What a list's child is now ([`Pass::child_now`]).
#[derive(Debug, PartialEq)]
pub(super) enum Now {
    /// Done, by this merged PR.
    Landed(String),
    /// Open, with no landing recorded: its own work goes on.
    Open,
    /// Closed with none, continued as this sibling on its list if named.
    Closed(Option<String>),
    /// Closed as continued in this issue, `OWNER/REPO#N`, still open.
    Continues(String),
}

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
        && crate::record::end(job, &turn).as_deref() == Some("done")
        && !crate::record::stopped(job, &turn)
        && !job.turn_file(&turn, TurnFile::Timeout).exists()
        && !crate::ask::host_path(job, "question-needs-owner").exists()
}

/// What a closed issue's closure is: when it closed, and what it said then.
fn closure(issue: &Value) -> Value {
    json!({"closed_at": issue["closed_at"], "title": issue["title"], "body": issue["body"]})
}

/// Every PR or issue the job's turns awaited, newest first, as `OWNER/REPO#N`.
fn awaited_targets(job: &Job) -> Vec<String> {
    let turns: u64 = job.turns().and_then(|t| t.parse().ok()).unwrap_or(0);
    let mut seen = std::collections::BTreeSet::new();
    (1..=turns)
        .rev()
        .flat_map(|turn| {
            let text =
                read(&job.turn_file(&turn.to_string(), TurnFile::GithubAwaits)).unwrap_or_default();
            text.lines()
                .rev()
                .filter_map(|l| reference(l.split('\t').next().unwrap_or_default()))
                .map(|(r, n)| format!("{r}#{n}"))
                .collect::<Vec<_>>()
        })
        .filter(|t| seen.insert(t.to_lowercase()))
        .collect()
}

/// The successor a comment's text names ([`CONTINUED`]), `#N` being in `repo`.
fn continued_as(repo: &str, body: &str) -> Option<(String, u64)> {
    body.lines().find_map(|l| {
        let named = l
            .trim()
            .strip_prefix(CONTINUED)?
            .split_whitespace()
            .next()?;
        let named = named.trim_end_matches(['.', ',', ':', ';']);
        match named.strip_prefix('#') {
            Some(n) => reference(&format!("{repo}#{n}")),
            None => reference(named),
        }
    })
}

/// `https://github.com/OWNER/REPO/pull/N` as `OWNER/REPO#N`.
fn pull_url(s: &str) -> Option<(String, u64)> {
    let path = s.trim().strip_prefix("https://github.com/")?;
    let (repo, n) = path.trim_end_matches('/').rsplit_once("/pull/")?;
    reference(&format!("{repo}#{n}"))
}

impl Pass<'_> {
    /// The merged PR tied to child N: Some(Some(url)) when there is one,
    /// Some(None) when there's none, None when there may be one GitHub
    /// couldn't be asked about. A source that can't be read never vetoes
    /// another that proves it. What a finished job awaited counts only for
    /// the closure it was kept on: `closed` is the child as GitHub has it
    /// now, when closed.
    pub(super) fn landed_child(
        &mut self,
        repo: &str,
        n: u64,
        closed: Option<&Value>,
    ) -> Option<Option<String>> {
        if let Some(pr) = self.recorded_landing(repo, n) {
            return Some(Some(pr));
        }
        // A child split again is done when its own list is, and only then.
        if let Ok(bytes) = fs::read(self.plan_dir(repo, &n.to_string()).join("tasks.json"))
            && let Ok(list) = serde_json::from_slice::<List>(&bytes)
            && !list.children.is_empty()
        {
            return Some(
                self.split_done(repo, n, list)?
                    .then(|| format!("https://github.com/{repo}/issues/{n}")),
            );
        }
        // What its finished job awaited, then the PR it was split to carry,
        // which counts whatever its state and however it closed.
        let mut targets = closed
            .map(|issue| self.awaited_for(repo, n, issue))
            .unwrap_or_default();
        targets.extend(self.carried(repo, n));
        let mut unsure = false;
        match self.first_merged(&targets) {
            Some(Some(pr)) => return Some(Some(pr)),
            Some(None) => {}
            None => unsure = true,
        }
        if closed.is_some() && !targets.is_empty() {
            self.book(repo, crate::now() + AWAITED_LOOK);
        }
        // A closing reference, from whichever repo.
        match self.closing_reference(repo, n) {
            Some(Some(pr)) => Some(Some(pr)),
            Some(None) if !unsure => Some(None),
            _ => None,
        }
    }

    /// Whether split child N's own `list` is done, read from its children
    /// as GitHub has them now, each by the rule its own pass reads it by
    /// ([`Pass::child_now`]), not only from the ticks a pass of it kept: its
    /// list is sequenced only while N is open, and a PR may close N with its
    /// last child (FieldmouseWorks/Conary#416). Like that pass, it reads none
    /// while N's own job is live or once N changed after its split. The
    /// ticks it finds are kept
    /// on the list and its checklist, and an open child it ticks is closed.
    /// None when one couldn't be asked about.
    fn split_done(&mut self, repo: &str, n: u64, mut list: List) -> Option<bool> {
        if list.next().is_none() {
            return Some(true);
        }
        // Walked only as its own pass would walk it: not while its own job
        // holds it, nor once it changed after its split.
        if self.owner(repo, &n.to_string()).is_some() {
            return Some(false);
        }
        let split = api(self.cfg, &format!("repos/{repo}/issues/{n}"))?;
        if !self.eligible(repo, &split) || !self.verified_source(repo, &split)? {
            return Some(false);
        }
        let at = format!("{repo}#{n}").to_lowercase();
        if self.splits.contains(&at) {
            return Some(false);
        }
        self.splits.push(at);
        let refs = list.refs(repo, &n.to_string());
        let mut ticked = false;
        let done = loop {
            let Some(index) = list.next() else {
                break Some(true);
            };
            let Some(child) = list.children[index].number else {
                break Some(false);
            };
            let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{child}")) else {
                break None;
            };
            if !self.eligible(repo, &issue) {
                break Some(false);
            }
            match self.child_now(repo, child, &issue, &refs) {
                Some(Now::Landed(pr)) => {
                    // An open one recorded so is closed with it, as its
                    // list's own pass would.
                    if issue["state"] != "closed"
                        && !self.close(repo, &child.to_string(), "completed")
                    {
                        break None;
                    }
                    let at = &mut list.children[index];
                    at.merged_pr = Some(pr);
                    at.waits = false;
                    at.continued = None;
                    ticked = true;
                }
                Some(_) => break Some(false),
                None => break None,
            }
        };
        self.splits.pop();
        if ticked {
            if !self.save_list(repo, &n.to_string(), &list) {
                self.retry(repo);
            } else if !self.render_list(repo, &n.to_string(), &mut list) {
                self.miss(repo);
            }
        }
        done
    }

    /// What child `child` of a list is now, `issue` being it as GitHub has
    /// it and `refs` its list ([`List::refs`]): the one rule every list's
    /// child is read by. An open one is done only by a landing an author
    /// recorded; a closed one by a merged PR tied to it, or its successor's.
    /// None when it couldn't be told.
    pub(super) fn child_now(
        &mut self,
        repo: &str,
        child: u64,
        issue: &Value,
        refs: &[String],
    ) -> Option<Now> {
        if issue["state"] != "closed" {
            return Some(
                self.recorded_landing(repo, child)
                    .map_or(Now::Open, Now::Landed),
            );
        }
        if let Some(pr) = self.landed_child(repo, child, Some(issue))? {
            return Some(Now::Landed(pr));
        }
        match self.successor(repo, issue, refs) {
            Successor::Landed(pr) => Some(Now::Landed(pr)),
            Successor::None => Some(Now::Closed(None)),
            Successor::OnList(sibling) => Some(Now::Closed(Some(sibling))),
            Successor::Open(to) => Some(Now::Continues(to)),
            Successor::Unknown => None,
        }
    }

    /// Where closed child `issue` (as REST gives it) went on: the successor
    /// its newest author's comment naming one names, in one of this
    /// machine's projects and off its own list (`list`, the parent and its
    /// children, as `OWNER/REPO#N`; one of those named is [`Successor::OnList`]).
    /// Done as a child is ([`Pass::landed_child`]);
    /// one closed without that passes on to the successor it names in turn.
    /// Comments count as [`Pass::written`] verifies them: every writer and
    /// editor an author. While one is open the repo is looked at again in a
    /// while, since a merge elsewhere sends no event here.
    pub(super) fn successor(&mut self, repo: &str, issue: &Value, list: &[String]) -> Successor {
        let mut from = (repo.to_owned(), issue.clone());
        let list: std::collections::BTreeSet<String> =
            list.iter().map(|r| r.to_lowercase()).collect();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..HOPS {
            let Some(written) = self.written(&from.0, &from.1) else {
                return Successor::Unknown;
            };
            // The newest comment that names one is the one that counts,
            // whether or not what it names can be read.
            let Some((at, m)) = written
                .comments
                .iter()
                .rev()
                .map(|c| c["body"].as_str().unwrap_or_default())
                .find(|b| b.lines().any(|l| l.trim().starts_with(CONTINUED)))
                .and_then(|b| continued_as(&from.0, b))
            else {
                return Successor::None;
            };
            let Some(at) = self
                .project
                .keys()
                .find(|r| r.eq_ignore_ascii_case(&at))
                .cloned()
            else {
                return Successor::None;
            };
            let named = format!("{at}#{m}");
            if list.contains(&named.to_lowercase()) {
                return Successor::OnList(named);
            }
            if !seen.insert(named.to_lowercase()) {
                return Successor::None;
            }
            let got = crate::github::reported(burrow_github::call_allowing(
                &mut crate::github::client(self.cfg),
                "GET",
                &format!("repos/{at}/issues/{m}"),
                None,
                None,
                &[404, 410],
            ));
            let Some(got) = got else {
                return Successor::Unknown;
            };
            if got.status != 200 {
                return Successor::None;
            }
            let Ok(next) = got.json() else {
                return Successor::Unknown;
            };
            let closed = next["state"] == "closed";
            // A PR named as the successor is the work itself.
            if next.get("pull_request").is_some() {
                if let Some(pr) = merged(&next) {
                    return Successor::Landed(pr);
                }
                if closed {
                    return Successor::None;
                }
                self.book(repo, crate::now() + AWAITED_LOOK);
                return Successor::Open(format!("{at}#{m}"));
            }
            match self.landed_child(&at, m, closed.then_some(&next)) {
                Some(Some(pr)) => return Successor::Landed(pr),
                None => return Successor::Unknown,
                Some(None) if !closed => {
                    self.book(repo, crate::now() + AWAITED_LOOK);
                    return Successor::Open(format!("{at}#{m}"));
                }
                Some(None) => from = (at, next),
            }
        }
        Successor::None
    }

    fn closing_reference(&self, repo: &str, n: u64) -> Option<Option<String>> {
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

    /// Whether N is a child on any of the repo's task lists.
    pub(in crate::inbox) fn is_child(&self, repo: &str, n: u64) -> bool {
        self.plan_dirs(repo).iter().any(|d| {
            fs::read(d.join("tasks.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<List>(&bytes).ok())
                .is_some_and(|list| list.children.iter().any(|c| c.number == Some(n)))
        })
    }

    /// The PR child N was split to carry (a task's `carries`), as its list
    /// has it.
    fn carried(&self, repo: &str, n: u64) -> Option<String> {
        self.plan_dirs(repo).iter().find_map(|d| {
            let list: List = serde_json::from_slice(&fs::read(d.join("tasks.json")).ok()?).ok()?;
            let at = list.children.iter().position(|c| c.number == Some(n))?;
            list.tasks.get(at)?.carries.clone()
        })
    }

    /// Keep what child N's job awaited, once N is `closed` with that job's
    /// completion accepted: bound to that closure (when it closed, and the
    /// title and body it closed with), so a reopened, edited or reclosed
    /// child never inherits it, whether or not a pass saw it open.
    pub(in crate::inbox) fn keep_awaited(
        &self,
        repo: &str,
        n: u64,
        job: &Job,
        closed: &Value,
    ) -> bool {
        let targets = awaited_targets(job);
        if targets.is_empty() || closed["state"] != "closed" || !closed["closed_at"].is_string() {
            return true;
        }
        let kept = json!({"closure": closure(closed), "targets": targets});
        let d = self.plan_dir(repo, &n.to_string());
        fs::create_dir_all(&d).is_ok() && swap(&d.join(AWAITED), &kept.to_string()).is_ok()
    }

    /// What child N's finished job awaited, if it was kept on the closure
    /// `closed` is: none for another closure, or an unreadable record.
    fn awaited_for(&self, repo: &str, n: u64, closed: &Value) -> Vec<String> {
        read(&self.plan_dir(repo, &n.to_string()).join(AWAITED))
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(|kept| kept["closure"] == closure(closed))
            .and_then(|kept| serde_json::from_value(kept["targets"].clone()).ok())
            .unwrap_or_default()
    }

    /// The first of these that's a merged PR, as its URL. One that can't
    /// be read is passed over: None only when none has merged and one
    /// couldn't be read. One GitHub says isn't there (a mistyped `carries`,
    /// say) has not merged, so the list waits on it and says so.
    fn first_merged(&self, targets: &[String]) -> Option<Option<String>> {
        let mut unsure = false;
        for target in targets {
            let Some((at, m)) = reference(target) else {
                continue;
            };
            let got = crate::github::reported(burrow_github::call_allowing(
                &mut crate::github::client(self.cfg),
                "GET",
                &format!("repos/{at}/issues/{m}"),
                None,
                None,
                &[404, 410],
            ));
            match got {
                Some(got) if got.status != 200 => {}
                Some(got) => match got.json() {
                    Ok(issue) => {
                        if let Some(pr) = merged(&issue) {
                            return Some(Some(pr));
                        }
                    }
                    Err(_) => unsure = true,
                },
                None => unsure = true,
            }
        }
        (!unsure).then_some(None)
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
        let number = |t: Option<String>| t.and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
        if !declared_done(job) || number(files::answered(d)) < turns.parse::<i64>().unwrap_or(0) {
            return true;
        }
        let Some(written) = self.written(repo, issue) else {
            return false;
        };
        // The closure kept must be the text checked: a REST read that
        // disagrees with the verified one is read again next pass.
        if !written.matches(issue) {
            return false;
        }
        let seen = number(files::seen(d));
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
            || self.keep_awaited(repo, child, job, issue)
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
            // A scoped pass's list says nothing of a parent outside it.
            if !self.in_scope(&parent) {
                continue;
            }
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

    /// What a notice that the list waits may still be open about: the
    /// child's number, or [`PAUSED`].
    fn notice_owed(&self, repo: &str, parent: &str) -> Option<String> {
        read(&self.plan_dir(repo, parent).join(NOTICE))
    }

    /// Whether a notice may still be open about something other than
    /// `child`, the list's next child, closed with nothing landed.
    pub(super) fn notice_moved_on(&self, repo: &str, parent: &str, child: u64) -> bool {
        self.notice_owed(repo, parent)
            .is_some_and(|on| on != child.to_string())
    }

    /// Whether a notice may still be open about the parent's pause.
    pub(super) fn notice_paused(&self, repo: &str, parent: &str) -> bool {
        self.notice_owed(repo, parent).as_deref() == Some(PAUSED)
    }

    /// Tell the owner, through the notifier, while the list waits on a
    /// child closed with no landed PR or on its parent's pause, and clear
    /// that once it doesn't. Both are retried every pass until they hold:
    /// `ensure` is safe to repeat (and updates the notice's text), and a
    /// close counts only once `pending` says the notice is gone.
    pub(super) fn reconcile_notice(&mut self, repo: &str, parent: &str, waiting_on: Option<Stall>) {
        let Some(notify) = &self.cfg.notify else {
            return;
        };
        let owed = self.plan_dir(repo, parent).join(NOTICE);
        let id = format!("proj-plan-{}-{parent}", key(repo));
        let run = |args: &[&str]| Command::new(notify).args(args).status().ok();
        match waiting_on {
            Some(stall) => {
                let (on, title, body) = match stall {
                    Stall::Child { child, named } => (
                        child.to_string(),
                        format!("{repo}#{parent}'s task list waits on #{child}"),
                        format!(
                            "{repo}#{child} closed without a merged PR tied to it, so the task list on {repo}#{parent} waits and its next child doesn't start.{} If its work landed, an author records the PR with `proj plan done {repo}#{child} PR-URL`; if it went on elsewhere, an author comments `Continued as OWNER/REPO#N` on #{child}, naming an issue or PR that is neither {repo}#{parent} nor another child of its list; otherwise reopen it.",
                            named.map_or_else(String::new, |named| format!(
                                " Its newest `Continued as` names {named}, which is on this task list itself, so it hands nothing on."
                            )),
                        ),
                    ),
                    Stall::Paused => (
                        PAUSED.to_owned(),
                        format!("{repo}#{parent}'s task list is paused"),
                        format!(
                            "{repo}#{parent} changed after its split (its title, its body or an author's comment on it), so its task list is paused and none of its children starts. It goes on once the parent is replanned from its new text, or its text is back as it was split (docs/task-planning.md); the list and the work done on it are kept meanwhile. A `Continued as` or `proj plan done` on a child is read once it goes on."
                        ),
                    ),
                };
                // Kept before it's asked for, so a crash still owes a close.
                if swap(&owed, &format!("{on}\n")).is_err() {
                    eprintln!("proj-inbox: couldn't keep {}", owed.display());
                    return;
                }
                let said = run(&["ensure", &id, &title, &body]);
                if !said.is_some_and(|s| s.success()) {
                    eprintln!("proj-inbox: couldn't tell the owner {repo}#{parent} waits");
                    self.retry(repo);
                }
                // The notifier can't say it reached GitHub: while the list
                // waits, ensure it again in a while (safe to repeat).
                self.book(repo, crate::now() + AWAITED_LOOK);
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
    fn pull_urls() {
        assert_eq!(
            pull_url("https://github.com/sam/box/pull/10"),
            Some(("sam/box".into(), 10))
        );
        assert_eq!(pull_url("https://github.com/sam/box/issues/10"), None);
        assert_eq!(pull_url("https://example.com/sam/box/pull/10"), None);
    }

    #[test]
    fn continuations() {
        let said = "Continued as FieldmouseWorks/burrow#948, opened as an owner issue.";
        assert_eq!(
            continued_as("sam/box", said),
            Some(("FieldmouseWorks/burrow".into(), 948))
        );
        assert_eq!(
            continued_as("sam/box", "Done here.\n\nContinued as #12."),
            Some(("sam/box".into(), 12))
        );
        assert_eq!(continued_as("sam/box", "It was continued as #12."), None);
        assert_eq!(
            continued_as("sam/box", "Continued as soon as we can."),
            None
        );
    }
}
