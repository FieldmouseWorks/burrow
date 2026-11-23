//! When a task list's child is done: a merged PR is tied to it
//! (docs/task-planning.md), or an author recorded that its closure needed
//! none. Tied is a closing reference from any repo, a PR the child's own
//! job awaited before it said `proj done`, the one its task names it to
//! carry (`carries`, read from the parent's list), or one an author
//! recorded with `proj plan done`. The host keeps the second and the
//! author's records in the child's plan folder, and only the host writes
//! them: `awaited`, every PR or issue the job awaited, once the child is
//! closed and the pass has accepted the job's completion ([`declared_done`]
//! and the issue as it was handed it); `landed` and `no-pr`, from `proj
//! plan done` outside any job, under the inbox's pass lock (plan_done.rs).
//! An awaited one counts once it has merged, whenever that is, and a
//! reopened child drops what its earlier job awaited, as it drops a
//! `no-pr`: both count only on the closure they were kept on.
//!
//! A child closed as continued elsewhere (an author's comment on it with a
//! line `Continued as OWNER/REPO#N`) hands its place to that issue: the list
//! waits on it, saying so, and is done when a merged PR closes it. One
//! naming the parent or a child of its own list hands nothing on, and the
//! owner's notice says that it doesn't count.
use super::{CHANGED, List, Pass, key};
use crate::inbox::record::files;
use crate::planning::reference;
use crate::record::TurnFile;
use crate::{job::Job, read, swap};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

/// The child's landed PR, as an author recorded it.
const LANDED: &str = "landed";

/// What the child's finished job awaited, newest first, kept once the child
/// closed with that job's completion accepted.
const AWAITED: &str = "awaited";

/// Why a closed child needed no PR, as an author recorded it, bound to the
/// closure it was recorded on.
const NO_PR: &str = "no-pr";

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

/// What a list's child is now ([`Pass::child_now`]).
#[derive(Debug, PartialEq)]
pub(super) enum Now {
    /// Done, by this merged PR (or what an author recorded in its place).
    Landed(String),
    /// Open, with no landing recorded: its own work goes on.
    Open,
    /// Closed with none, continued as this issue on its own list if named.
    Closed(Option<String>),
    /// Closed as continued in this issue, `OWNER/REPO#N`, still open.
    Continues(String),
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

impl Pass<'_> {
    /// The merged PR tied to child N: Some(Some(url)) when there is one
    /// (or, for a closed child an author recorded as needing none, what the
    /// checklist says instead), Some(None) when there's none, None when
    /// there may be one GitHub couldn't be asked about. A source that can't be read never vetoes
    /// another that proves it. What a finished job awaited counts only for
    /// the closure it was kept on: `closed` is the child as GitHub has it
    /// now, when closed.
    pub(super) fn landed_child(
        &mut self,
        repo: &str,
        n: u64,
        closed: Option<&Value>,
    ) -> Option<Option<String>> {
        self.landed_at(repo, n, closed, &mut Vec::new())
    }

    /// [`Pass::landed_child`], `walking` the split children whose lists
    /// are being read above it, as `OWNER/REPO#N`.
    fn landed_at(
        &mut self,
        repo: &str,
        n: u64,
        closed: Option<&Value>,
        walking: &mut Vec<String>,
    ) -> Option<Option<String>> {
        if let Some(pr) = self.recorded_landing(repo, n) {
            return Some(Some(pr));
        }
        if let Some(done) = closed.and_then(|issue| self.done_without_pr(repo, n, issue)) {
            return Some(Some(done));
        }
        // A child split again is done when its own list is, and only then.
        if let Ok(bytes) = fs::read(self.plan_dir(repo, &n.to_string()).join("tasks.json"))
            && let Ok(list) = serde_json::from_slice::<List>(&bytes)
            && !list.children.is_empty()
        {
            return self.split_landed(repo, n, closed, list, walking);
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
    /// as GitHub has them now rather than from the ticks alone: its list
    /// stops moving once N closes ([`Pass::sequence`] reads open parents
    /// only), so a PR that closes N with its last child would leave it
    /// waiting for good (FieldmouseWorks/Conary#416). Each child counts as
    /// [`Pass::child_now`] says, the rule its list's own pass reads it by;
    /// one whose list is already being read above (a successor naming an
    /// ancestor) doesn't. What counts is ticked, kept and rendered, and an
    /// open child an author recorded landed is closed, only once N is
    /// `closed` (N as GitHub has it): an open N's list is
    /// [`Pass::sequence`]'s to write.
    fn split_landed(
        &mut self,
        repo: &str,
        n: u64,
        closed: Option<&Value>,
        mut list: List,
        walking: &mut Vec<String>,
    ) -> Option<Option<String>> {
        let at = format!("{repo}#{n}").to_lowercase();
        if walking.contains(&at) {
            return Some(None);
        }
        walking.push(at);
        let (done, ticked) = self.split_walk(repo, n, closed, &mut list, walking);
        walking.pop();
        if closed.is_some() {
            let parent = n.to_string();
            if ticked && !self.save_list(repo, &parent, &list) {
                self.retry(repo);
                return None;
            }
            // Nothing else renders a closed list: one that didn't go
            // through holds the answer until it does, as sequence's own.
            if !self.render_list(repo, &parent, &mut list) {
                self.miss(repo);
                return None;
            }
        }
        done
    }

    /// Ticks split child N's `list` on from its children as GitHub has
    /// them ([`Pass::split_landed`]): whether it's done, and whether any
    /// child was ticked. A list with a child left is walked only as its own
    /// pass ([`Pass::sequence`]) would move it: not while N's own job is
    /// live, nor once N changed after its split, which waits to be
    /// replanned. `closed` is N as GitHub has it, when closed.
    fn split_walk(
        &mut self,
        repo: &str,
        n: u64,
        closed: Option<&Value>,
        list: &mut List,
        walking: &mut Vec<String>,
    ) -> (Option<Option<String>>, bool) {
        let parent = n.to_string();
        let mut ticked = false;
        if list.next().is_some() {
            match self.walkable(repo, n, closed) {
                Some(true) => {}
                Some(false) => return (Some(None), ticked),
                None => return (None, ticked),
            }
        }
        let refs = list.refs(repo, &parent);
        while let Some(index) = list.next() {
            let Some(child) = list.children[index].number else {
                return (Some(None), ticked);
            };
            let Some(issue) = crate::github::api(self.cfg, &format!("repos/{repo}/issues/{child}"))
            else {
                return (None, ticked);
            };
            if !self.eligible(repo, &issue) {
                return (Some(None), ticked);
            }
            let pr = match self.child_now(repo, child, &issue, &refs, walking) {
                Some(Now::Landed(pr)) => pr,
                Some(_) => return (Some(None), ticked),
                None => return (None, ticked),
            };
            // An open one recorded landed is closed with it, as its list's
            // own pass would; an open N's children are that pass's to close.
            if issue["state"] != "closed" {
                if closed.is_none() {
                    return (Some(None), ticked);
                }
                if !self.close(repo, &child.to_string(), "completed") {
                    return (None, ticked);
                }
            }
            let at = &mut list.children[index];
            at.merged_pr = Some(pr);
            at.waits = false;
            at.continued = None;
            ticked = true;
        }
        let done = format!("https://github.com/{repo}/issues/{n}");
        (Some(Some(done)), ticked)
    }

    /// Whether split child N's list may move now, as [`Pass::sequence`]
    /// holds it: not while N's own job is live, nor once N (`issue`, as
    /// GitHub has it, fetched when not given) changed after its split.
    /// None when GitHub or N's plan couldn't be read.
    fn walkable(&mut self, repo: &str, n: u64, issue: Option<&Value>) -> Option<bool> {
        if self.owner(repo, &n.to_string()).is_some() {
            return Some(false);
        }
        let fetched;
        let issue = match issue {
            Some(issue) => issue,
            None => {
                fetched = crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}"))?;
                &fetched
            }
        };
        Some(self.eligible(repo, issue) && self.verified_source(repo, issue)?)
    }

    /// What child `child` of a list is now, `issue` being it as GitHub has
    /// it and `refs` its list ([`List::refs`]): the one rule every list's
    /// child is read by, its own list's pass ([`Pass::sequence`]) and a
    /// closed split child's walk ([`Pass::split_landed`]) alike. An open
    /// one is done only by a landing an author recorded; a closed one by a
    /// merged PR tied to it ([`Pass::landed_child`]), or its successor's.
    /// `walking` as [`Pass::landed_at`] is. None when it couldn't be told.
    pub(super) fn child_now(
        &mut self,
        repo: &str,
        child: u64,
        issue: &Value,
        refs: &[String],
        walking: &mut Vec<String>,
    ) -> Option<Now> {
        if issue["state"] != "closed" {
            return Some(
                self.recorded_landing(repo, child)
                    .map_or(Now::Open, Now::Landed),
            );
        }
        if let Some(pr) = self.landed_at(repo, child, Some(issue), walking)? {
            return Some(Now::Landed(pr));
        }
        match self.successor_at(repo, issue, refs, walking) {
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
    /// `walking` as [`Pass::landed_at`] is.
    fn successor_at(
        &mut self,
        repo: &str,
        issue: &Value,
        list: &[String],
        walking: &mut Vec<String>,
    ) -> Successor {
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
            match self.landed_at(&at, m, closed.then_some(&next), walking) {
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

    /// What the checklist says of closed child N, `closed` as GitHub has
    /// it, when an author recorded that it needed no PR on that closure.
    fn done_without_pr(&self, repo: &str, n: u64, closed: &Value) -> Option<String> {
        let kept: Value =
            serde_json::from_str(&read(&self.plan_dir(repo, &n.to_string()).join(NO_PR))?).ok()?;
        (kept["closure"] == closure(closed)).then_some(())?;
        let reason = kept["reason"].as_str().filter(|r| !r.trim().is_empty())?;
        Some(format!("no PR needed: {}", reason.trim()))
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
                            "{repo}#{child} closed without a merged PR tied to it, so the task list on {repo}#{parent} waits and its next child doesn't start.{} If its work landed, an author records the PR with `proj plan done {repo}#{child} PR-URL`; if its record was the deliverable and no PR was needed, an author records why with `proj plan done {repo}#{child} --no-pr REASON`; if it went on elsewhere, an author comments `Continued as OWNER/REPO#N` on #{child}, naming an issue or PR that is neither {repo}#{parent} nor another child of its list; otherwise reopen it.",
                            named.map_or_else(String::new, |named| format!(
                                " Its newest `Continued as` names {named}, which is on this task list itself, so it hands nothing on."
                            )),
                        ),
                    ),
                    Stall::Paused => (
                        PAUSED.to_owned(),
                        format!("{repo}#{parent}'s task list is paused"),
                        format!(
                            "{repo}#{parent} {CHANGED}, so its task list is paused and none of its children starts. It goes on once the parent is replanned from its new text, or its text is back as it was split; an author ends it without closing the parent with `proj plan end {repo}#{parent}` (docs/task-planning.md). The list and the work done on it are kept meanwhile. No replan starts while a live job's brief names the parent: `proj why {repo}#{parent}` names the job. A `Continued as` or `proj plan done` on a child is read once it goes on."
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
}

#[path = "plan_done.rs"]
mod door;
pub(crate) use door::PLAN_USAGE;

/// A merged PR's URL, from its issue as REST gives it.
fn merged(issue: &Value) -> Option<String> {
    let pr = issue.get("pull_request")?;
    pr["merged_at"].as_str().filter(|t| !t.is_empty())?;
    pr["html_url"]
        .as_str()
        .or_else(|| issue["html_url"].as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod parse_tests {
    use super::*;

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
