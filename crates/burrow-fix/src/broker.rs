//! The broker (Conary's `docs/m6-plugins.md`, F): one fix request a
//! connection, from the flow's controller alone (burrow-producer's socket).
//!
//! - It checks the request's head against the PR's on GitHub.
//! - It checks the fixing role would run as the flow chose, on the engine
//!   the plugin names itself by, before anything starts.
//! - It starts the fix as a proj job on the fixing role, under a key of its
//!   own beneath the PR's. At most one live fix runs a PR: one held here,
//!   or one a request was waiting on when the broker last stopped
//!   ([`Broker::reconcile`]).
//! - It waits on the job through its record, books its own deadline from
//!   the record's `overdue_at`, and stops the job when the request ends
//!   first (the plugin killed, past its deadline, cancelled).
//! - It answers with what the job said, held to the problems it was handed
//!   and to the head the PR has on GitHub after it: a push is a new commit
//!   on the head it was handed.
//!
//! It serves the stuck role's `job` the same way ([`Broker::handle_job`],
//! the handoff in [`crate::stuck`]): the PR read from GitHub, the job on
//! the stuck role, one live a PR, stopped with its request, and its answer
//! held to the head GitHub shows and the follow-up issue it names. The
//! broker finds that issue before the job starts, by its marker among the
//! issues the flow's own accounts opened ([`Broker::find_followup`]), and
//! hands the job its number: the job never looks for it, so it never reads
//! or edits one anyone else opened.
//!
//! The fixes it's waiting on are kept in `live/`, by their keys, with the
//! head each was handed and what it was asked, until each ends. A job is a
//! proj job and outlives the broker: a restart leaves it at work and
//! re-adopts it ([`Broker::reconcile`]). The flow, its connection lost,
//! asks again; a request that asks the same of the same PR is answered from
//! that job, at work or ended, and starts no second one. One for a head
//! the PR has moved to supersedes it, as the flow's closed connection
//! would have: the job is stopped and the request starts its own. One that
//! asks anything else of the same head is refused while that job is at
//! work, as beside any live fix, and once it has ended, starts afresh. A
//! re-adopted job is stopped by nothing else: no one asking, it ends by
//! its own turn's timeout and idle limit, which proj keeps. A push that already happened stands; the next pass reads the
//! head that moved.
use crate::answers::Said;
use crate::stuck::{self, Ask, Outcome};
use crate::{Config, brief, discussion};
use burrow_github::{Api, call, pages};
use burrow_producer::jobs::{self, Jobs};
use burrow_producer::serve::Work;
use conary_core::plugin::{
    Answer, Context, Executed, FixResult, Identity, Next, Request, Response, Task, version,
};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A GitHub to ask, new for each look.
pub type GitHub = Box<dyn Fn() -> Box<dyn Api> + Send + Sync>;

pub struct Broker {
    pub cfg: Config,
    pub jobs: Box<dyn Jobs>,
    /// The runner its stuck-role jobs start on, when it serves them. proj
    /// finds, reads and stops a job by its key or id whatever role it runs
    /// on, so only their start is this one's: the rest is [`Broker::jobs`]'s.
    pub stuck: Option<Box<dyn Jobs>>,
    pub github: GitHub,
    /// How often it reads a job's record while it waits.
    pub look: Duration,
    /// The PRs with a live fix here, by [`key`].
    live: Mutex<HashMap<String, Live>>,
}

/// A PR's live work, as the broker holds it.
enum Live {
    /// A request here is waiting on its job.
    Asked,
    /// The job of a request that ended, which wouldn't stop
    /// ([`Held::keep`]): stopped before the PR's next job starts.
    Kept(String),
    /// The job a request was waiting on when the broker last stopped,
    /// re-adopted at start ([`Broker::reconcile`]): the one a request
    /// that asks the same joins.
    Adopted(Entry),
}

/// What `live/` keeps of a PR's job, a line each: its proj key, the head
/// it was handed, what it was asked ([`asked`]), and `kept` once its
/// request ended and it wouldn't stop. A head or ask left out (as an older
/// broker wrote it, the key alone) is one no request asks again.
#[derive(Debug, Default, PartialEq)]
struct Entry {
    job: String,
    head: Option<String>,
    asked: Option<String>,
    kept: bool,
}

impl Entry {
    fn read(text: &str) -> Self {
        let mut lines = text.lines().map(str::trim);
        let mut next = || lines.next().filter(|l| !l.is_empty()).map(str::to_owned);
        let job = next().unwrap_or_default();
        let (head, asked) = (next(), next());
        Self {
            job,
            head,
            asked,
            kept: next().as_deref() == Some("kept"),
        }
    }

    fn write(&self) -> String {
        let line = |l: &Option<String>| l.clone().unwrap_or_default();
        format!(
            "{}\n{}\n{}\n{}",
            self.job,
            line(&self.head),
            line(&self.asked),
            if self.kept { "kept\n" } else { "" }
        )
    }
}

/// What a request asks of its PR, as one line, for [`Entry::asked`]: two
/// requests are the same when these are, so a re-made request joins the
/// job of the one it repeats. A fix is its head and its problems, since
/// its job answers the findings it was handed by their index; a stuck-role
/// job is its prompt.
fn asked(what: &impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string(what).map_err(|e| format!("a request that won't write: {e}"))
}

/// A PR's live fix, here, and the proj key its job is held under: let go
/// when its request ends, however it ends, unless its job may still be at
/// work ([`Held::keep`]).
struct Held<'a> {
    broker: &'a Broker,
    pr: String,
    job: String,
    /// The job it joined, re-adopted from before a restart: its id and the
    /// head it was handed. None when this request starts its own.
    joined: Option<(String, Option<String>)>,
}

impl Held<'_> {
    /// Kept on disk until its request ends, with what it was handed and
    /// asked, so a restart re-adopts its job ([`Broker::reconcile`]).
    fn record(&self, head: &str, asked: &str) -> Result<(), String> {
        let kept = self.broker.kept(&self.job);
        let entry = Entry {
            job: self.job.clone(),
            head: Some(head.to_owned()),
            asked: Some(asked.to_owned()),
            kept: false,
        };
        fs::write(&kept, entry.write()).map_err(|e| format!("{}: {e}", kept.display()))
    }

    /// Its job wouldn't stop: the PR stays held by its job's key, kept on
    /// disk too, so no second fix starts beside it until that job is
    /// stopped, here or after a restart ([`Broker::hold`]).
    fn keep(self) {
        let entry = Entry {
            job: self.job.clone(),
            kept: true,
            ..Entry::default()
        };
        let _ = fs::write(self.broker.kept(&self.job), entry.write());
        if let Ok(mut live) = self.broker.live.lock() {
            live.insert(self.pr.clone(), Live::Kept(self.job.clone()));
        }
        std::mem::forget(self);
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.broker.kept(&self.job));
        if let Ok(mut live) = self.broker.live.lock() {
            live.remove(&self.pr);
        }
    }
}

/// Whether `issue`, as GitHub's REST gives it, may be `repo`'s PR `pr`'s
/// follow-up: an open issue, not a PR, with the PR's marker in its body,
/// that one of `accounts`' follow-up openers opened. Why not, if it isn't.
fn followup_of(
    issue: &serde_json::Value,
    repo: &str,
    pr: u64,
    accounts: &discussion::Accounts,
) -> Result<(), String> {
    if issue.get("pull_request").is_some_and(|p| !p.is_null()) {
        return Err("is a PR, not an issue".into());
    }
    if issue["state"] != "open" {
        return Err("isn't open".into());
    }
    match issue["user"]["login"].as_str() {
        Some(login) if accounts.opened(login) => {}
        by => {
            return Err(format!(
                "was opened by {}, not by one of {:?}, the accounts a follow-up is opened as",
                by.unwrap_or("no one GitHub names"),
                accounts.followup
            ));
        }
    }
    if !issue["body"]
        .as_str()
        .is_some_and(|b| stuck::marks(b, repo, pr))
    {
        return Err(format!(
            "isn't {repo}#{pr}'s: its body has no line {}",
            stuck::marker(repo, pr)
        ));
    }
    Ok(())
}

/// The PR key a job key is beneath.
fn pr_of(job: &str) -> &str {
    job.rsplit_once('/').map_or(job, |(pr, _)| pr)
}

/// A PR's own key for a fix, in proj's key alphabet (letters, digits and
/// `._/:-`). GitHub's names have no `:`, so an `_` written as one can't
/// make two PRs' keys the same.
pub fn key(repo: &str, pr: u64) -> String {
    pr_key("fix", repo, pr)
}

/// A PR's own key for its stuck-role job: beside its fix's, never the same.
pub fn stuck_key(repo: &str, pr: u64) -> String {
    pr_key("stuck", repo, pr)
}

fn pr_key(kind: &str, repo: &str, pr: u64) -> String {
    format!(
        "{kind}/{}/{pr}",
        repo.to_ascii_lowercase().replace('_', ":")
    )
}

/// The proj key one request's job is held under: the PR's (`pr`), and a
/// part of its own, since proj never lets go of a key a job that ran
/// holds, and a PR is handed on again and again.
fn job_key(pr: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{pr}/{now:x}")
}

impl Broker {
    pub fn new(cfg: Config, jobs: Box<dyn Jobs>, github: GitHub) -> io::Result<Self> {
        fs::create_dir_all(cfg.state.join("live"))?;
        Ok(Self {
            cfg,
            jobs,
            stuck: None,
            github,
            look: Duration::from_secs(10),
            live: Mutex::default(),
        })
    }

    /// Where a live fix's key is kept until it ends.
    fn kept(&self, key: &str) -> PathBuf {
        self.cfg.state.join("live").join(key.replace('/', "+"))
    }

    /// At startup: hold each PR whose job was live when the broker stopped,
    /// as it was held before. A job a request was waiting on is re-adopted,
    /// never stopped: it's a proj job, still at work or ended, and the
    /// flow's next request for the same work is answered from it
    /// ([`Broker::hold`]). One whose request had ended and that wouldn't
    /// stop is still stopped before the PR's next job. How many it holds.
    pub fn reconcile(&self) -> Result<usize, String> {
        let dir = self.cfg.state.join("live");
        let mut live = self
            .live
            .lock()
            .map_err(|_| "the broker's lock is poisoned")?;
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let entry = Entry::read(&text);
            let pr = pr_of(&entry.job).to_owned();
            live.insert(
                pr,
                if entry.kept {
                    Live::Kept(entry.job)
                } else {
                    Live::Adopted(entry)
                },
            );
        }
        Ok(live.len())
    }

    /// Stop the job held under `job`, if there is one: whether there was.
    fn let_go(&self, job: &str) -> Result<bool, String> {
        match self.jobs.find(job)? {
            Some(id) => self.jobs.stop(&id).map(|()| true),
            None => Ok(false),
        }
    }

    /// Why a request's fix ended with no answer, once its job (by `id`, or
    /// by its key when its start may have run all the same) is stopped; or,
    /// when it won't stop, its PR kept held.
    fn ended(&self, held: Held<'_>, id: Option<&str>, why: String) -> String {
        let stopped = match id {
            Some(id) => self.jobs.stop(id),
            None => self.let_go(&held.job).map(|_| ()),
        };
        match stopped {
            Ok(()) => why,
            Err(e) => {
                held.keep();
                format!("{why}; and its job wouldn't stop: {e}")
            }
        }
    }

    /// One request's fix, or its stuck-role job ([`Broker::handle_job`]):
    /// its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
        if matches!(request.task, Task::Job { .. }) {
            return self.handle_job(request, work);
        }
        if version(&request.contract).is_none_or(|v| v < 4) {
            return Err(format!(
                "{} names no PR: a fix needs conary-plugin v4",
                request.contract
            ));
        }
        let Task::Fix {
            repo,
            pr,
            head,
            problems,
        } = &request.task
        else {
            return Err("the fix broker does fixes alone".into());
        };
        if *pr == 0 {
            return Err("a fix request names its PR".into());
        }
        if let Some(e) = &request.execution
            && e.context != Context::Fresh
        {
            return Err("a fix job starts fresh: it resumes no session".into());
        }
        jobs::runnable(
            &self.cfg.settings,
            &self.cfg.role,
            repo,
            &jobs::offered(&self.cfg.settings, &self.cfg.role)?,
            request.execution.as_ref().and_then(|e| e.choice.as_ref()),
        )?;
        let asks = asked(&(head, problems))?;
        let (now, branch) = self.pr(repo, *pr)?;
        let current = (now == *head).then_some(head.as_str());
        let mut held = self.hold(
            key(repo, *pr),
            &format!("a fix of {repo}#{pr}"),
            &asks,
            current,
        )?;
        // The job it repeats was handed this head, which it may have moved
        // since: what it pushed is held to that head below, as its own
        // request's would have been.
        let id = match held.joined.take() {
            Some((id, _)) => id,
            None => {
                if current.is_none() {
                    return Err(format!(
                        "{repo}#{pr}'s head is {now}, not the {head} the fix was asked for"
                    ));
                }
                let task = brief::task(repo, *pr, head, &branch, problems, &self.cfg.jobs);
                held.record(head, &asks)?;
                // Stopped before the answer is let go of, so no fix is left
                // at work for a request that's over: a start that read as
                // failed may have started all the same.
                match self.jobs.start(repo, &held.job, &task) {
                    Ok(id) => id,
                    Err(why) => return Err(self.ended(held, None, why)),
                }
            }
        };
        let record = match jobs::wait(&*self.jobs, &id, work, self.look) {
            Ok(record) => record,
            Err(why) => return Err(self.ended(held, Some(&id), why)),
        };
        let said = self
            .jobs
            .answers(&id)?
            .ok_or_else(|| format!("the fix job {id} left no answers"))?;
        let (result, answers) = Said::read(&said)?.settle(problems)?;
        let now = self.pr(repo, *pr)?.0;
        match &result {
            FixResult::Pushed { head: pushed } if *pushed != now => {
                return Err(format!(
                    "the fix job says it pushed {pushed}, and the PR's head is {now}"
                ));
            }
            FixResult::Pushed { head: pushed } => self.pushed_on(repo, head, pushed)?,
            FixResult::Unchanged if now != *head => {
                return Err(format!(
                    "the fix job pushed nothing, and the PR's head moved from {head} to {now}"
                ));
            }
            _ => {}
        }
        let turn = record
            .turn
            .ok_or_else(|| format!("the fix job {id} ran no turn"))?;
        let engine = turn
            .engine
            .ok_or_else(|| format!("the fix job {id}'s turn names no engine"))?;
        drop(held);
        Ok(Response {
            contract: request.contract,
            by: Identity {
                engine,
                session: turn.session,
            },
            execution: request.execution.map(|_| Executed {
                model: turn.model,
                effort: turn.effort,
                context: Context::Fresh,
            }),
            answer: Answer::Fix {
                result,
                answers: Some(answers),
            },
        })
    }

    /// The PR's live work under `key` ([`key`], [`stuck_key`]) for a
    /// request that `asks` ([`asked`]) of the head its PR has on GitHub,
    /// `now` (none when it asks of another head), here, or why there's `what` at work already. A job re-adopted
    /// from before a restart that was asked the same is joined
    /// ([`Held::joined`]), at work or ended. One handed a head other than
    /// `now` was superseded, as the flow's closed connection would have
    /// said: it's stopped and let go of. One asked anything else on `now`
    /// holds the PR while it's at work, as a live request's job would, and
    /// is let go of once it has ended. A re-adopted job is stopped by
    /// nothing else but its own timeout and idle limit.
    fn hold(
        &self,
        key: String,
        what: &str,
        asks: &str,
        now: Option<&str>,
    ) -> Result<Held<'_>, String> {
        let mut live = self
            .live
            .lock()
            .map_err(|_| "the broker's lock is poisoned")?;
        let (mut job, mut joined, mut kept) = (job_key(&key), None, None);
        match live.get(&key) {
            None => {}
            Some(Live::Asked) => return Err(format!("{what} is at work already")),
            // Work whose job wouldn't stop: stopped now, or still held.
            Some(Live::Kept(job)) => {
                self.let_go(job).map_err(|e| {
                    format!("{what} is at work already, and its job {job} won't stop: {e}")
                })?;
                let _ = fs::remove_file(self.kept(job));
            }
            Some(Live::Adopted(entry)) => {
                let found = self
                    .jobs
                    .find(&entry.job)
                    .map_err(|e| format!("{what} may be at work already, as {}: {e}", entry.job))?;
                match found {
                    Some(id) if entry.asked.as_deref() == Some(asks) => {
                        job.clone_from(&entry.job);
                        joined = Some((id, entry.head.clone()));
                    }
                    Some(id)
                        if now
                            .is_some_and(|now| entry.head.as_deref().is_some_and(|h| h != now)) =>
                    {
                        if let Err(e) = self.superseded(&id) {
                            kept = Some((entry.job.clone(), e));
                        }
                    }
                    Some(id) => self.outlived(&id, what)?,
                    // It never started: nothing to join or stop.
                    None => {}
                }
                if joined.is_none() && kept.is_none() {
                    let _ = fs::remove_file(self.kept(&entry.job));
                }
            }
        }
        // A superseded job that wouldn't stop holds its PR as any kept job
        // does: stopped before the PR's next job starts.
        if let Some((job, e)) = kept {
            let entry = Entry {
                job: job.clone(),
                kept: true,
                ..Entry::default()
            };
            let _ = fs::write(self.kept(&job), entry.write());
            live.insert(key, Live::Kept(job.clone()));
            return Err(format!(
                "{what} is at work already, on a head the PR has moved past, and its job {job} won't stop: {e}"
            ));
        }
        live.insert(key.clone(), Live::Asked);
        Ok(Held {
            broker: self,
            job,
            pr: key,
            joined,
        })
    }

    /// Stop the re-adopted job `id`, handed a head the PR has moved past,
    /// unless its turn ended done.
    fn superseded(&self, id: &str) -> Result<(), String> {
        if self.jobs.why(id)?.done() {
            return Ok(());
        }
        self.jobs.stop(id)
    }

    /// A re-adopted job `id` that a request asking something else of its PR
    /// finds: why there's `what` at work already while it's at work; once
    /// its turn has ended, nothing, after stopping it if it ended holding
    /// anything open (a question, an await), so it can't take up again
    /// beside the PR's next job.
    fn outlived(&self, id: &str, what: &str) -> Result<(), String> {
        let record = self
            .jobs
            .why(id)
            .map_err(|e| format!("{what} may be at work already, as {id}: {e}"))?;
        if record.live() {
            return Err(format!(
                "{what} is at work already, as {id}, from before the broker's restart"
            ));
        }
        if !record.done() {
            self.jobs.stop(id).map_err(|e| {
                format!(
                    "{what} ended {}, and its job {id} won't stop: {e}",
                    record.stage
                )
            })?;
        }
        Ok(())
    }

    /// One request for the stuck role: its answer, or why there's none.
    pub fn handle_job(&self, request: Request, work: &Work) -> Result<Response, String> {
        let starter = self
            .stuck
            .as_deref()
            .ok_or("the broker serves no job kind: no stuck role is configured")?;
        let role = self
            .cfg
            .stuck
            .as_deref()
            .ok_or("the broker serves no job kind: no stuck role is configured")?;
        let Task::Job {
            repo,
            workdir,
            prompt,
            model,
            effort,
        } = &request.task
        else {
            return Err("the stuck role does jobs alone".into());
        };
        let ask = Ask::read(workdir, prompt)?;
        let execution = request.execution.as_ref().ok_or_else(|| {
            format!(
                "{} carries no execution: a job needs conary-plugin v3",
                request.contract
            )
        })?;
        if execution.context != Context::Fresh {
            return Err("a stuck-role job starts fresh: it resumes no session".into());
        }
        // proj runs the job on the stuck role's route and tuning, so the
        // flow's choice is held to them before it starts. The request's own
        // model and effort are in that choice, as Conary's runner sends
        // them: one that isn't is refused.
        let choice = execution.choice.as_ref();
        if model
            .as_ref()
            .is_some_and(|m| choice.and_then(|c| c.model.as_ref()) != Some(m))
            || effort
                .as_ref()
                .is_some_and(|e| choice.and_then(|c| c.effort.as_ref()) != Some(e))
        {
            return Err("the job's model and effort aren't its execution's choice".into());
        }
        jobs::runnable(
            &self.cfg.settings,
            role,
            repo,
            &jobs::offered(&self.cfg.settings, role)?,
            choice,
        )?;
        let pr = ask.pr;
        let flow = self
            .cfg
            .flow
            .as_deref()
            .ok_or("the broker has no flow configurations to read a repo's authors from")?;
        let accounts = discussion::accounts(flow, repo)?;
        let asks = asked(prompt)?;
        let (now, branch) = self.pr(repo, pr)?;
        let mut held = self.hold(
            stuck_key(repo, pr),
            &format!("a stuck-role job on {repo}#{pr}"),
            &asks,
            Some(&now),
        )?;
        let (id, head, handed) = match held.joined.take() {
            // The job it repeats, held to the head it was handed and to the
            // follow-up it would have been handed, which is still the
            // first of the PR's open ones.
            Some((id, head)) => {
                let head = head.ok_or_else(|| format!("the stuck-role job {id} names no head"))?;
                (id, head, self.find_followup(repo, pr, &accounts)?)
            }
            None => {
                let head = now;
                let shown =
                    discussion::read(&mut *(self.github)(), repo, pr, &accounts.discussion)?;
                let handed = self.find_followup(repo, pr, &accounts)?;
                let task = brief::stuck(
                    repo,
                    pr,
                    &head,
                    &branch,
                    &ask,
                    &shown,
                    handed,
                    &self.cfg.jobs,
                );
                held.record(&head, &asks)?;
                match starter.start(repo, &held.job, &task) {
                    Ok(id) => (id, head, handed),
                    Err(why) => return Err(self.ended(held, None, why)),
                }
            }
        };
        let record = match jobs::wait(&*self.jobs, &id, work, self.look) {
            Ok(record) => record,
            Err(why) => return Err(self.ended(held, Some(&id), why)),
        };
        let said = self
            .jobs
            .answers(&id)?
            .ok_or_else(|| format!("the stuck-role job {id} left no answers"))?;
        let said = stuck::Said::read(&said)?;
        let outcome = said.outcome()?;
        let now = self.pr(repo, pr)?.0;
        match &outcome {
            Outcome::Pushed(pushed) if *pushed != now => {
                return Err(format!(
                    "the stuck-role job says it pushed {pushed}, and the PR's head is {now}"
                ));
            }
            Outcome::Pushed(pushed) => self.pushed_on(repo, &head, pushed)?,
            Outcome::Unchanged if now != head => {
                return Err(format!(
                    "the stuck-role job pushed nothing, and the PR's head moved from {head} to {now}"
                ));
            }
            _ => {}
        }
        let followup = said
            .followup
            .map(|n| self.followup(repo, pr, n, handed, &accounts))
            .transpose()?;
        let turn = record
            .turn
            .ok_or_else(|| format!("the stuck-role job {id} ran no turn"))?;
        let engine = turn
            .engine
            .ok_or_else(|| format!("the stuck-role job {id}'s turn names no engine"))?;
        drop(held);
        Ok(Response {
            contract: request.contract,
            by: Identity {
                engine,
                session: turn.session,
            },
            execution: Some(Executed {
                model: turn.model,
                effort: turn.effort,
                context: Context::Fresh,
            }),
            answer: Answer::Job {
                summary: stuck::summary(&outcome, followup.as_deref(), &said.summary),
                next: Next::Done,
            },
        })
    }

    /// `repo`'s PR `pr`'s follow-up issue, if it has one: the first opened
    /// of the open issues that carry its marker and that one of
    /// `accounts`' follow-up openers opened. An issue anyone else opened is
    /// never one, whatever its body says.
    fn find_followup(
        &self,
        repo: &str,
        pr: u64,
        accounts: &discussion::Accounts,
    ) -> Result<Option<u64>, String> {
        let mut api = (self.github)();
        let open = pages(
            &mut *api,
            &format!("repos/{repo}/issues?state=open&per_page=100"),
        )
        .map_err(|e| format!("{repo}'s open issues can't be read: {e}"))?;
        Ok(open
            .iter()
            .flat_map(|page| page.as_array().cloned().unwrap_or_default())
            .filter(|i| followup_of(i, repo, pr, accounts).is_ok())
            .filter_map(|i| i["number"].as_u64())
            .min())
    }

    /// The follow-up issue `n` of `repo`'s PR `pr`, by its link, once
    /// GitHub shows it's the one the job was `handed`, if it was handed one,
    /// and an open issue (not a PR) with the PR's marker that one of
    /// `accounts`' follow-up openers opened.
    fn followup(
        &self,
        repo: &str,
        pr: u64,
        n: u64,
        handed: Option<u64>,
        accounts: &discussion::Accounts,
    ) -> Result<String, String> {
        if let Some(handed) = handed
            && handed != n
        {
            return Err(format!(
                "the stuck-role job names {repo}#{n} as the follow-up, and it was handed {repo}#{handed}"
            ));
        }
        let mut api = (self.github)();
        let got = call(
            &mut *api,
            "GET",
            &format!("repos/{repo}/issues/{n}"),
            None,
            None,
        )
        .and_then(|r| r.json())
        .map_err(|e| format!("the follow-up issue {repo}#{n} can't be read: {e}"))?;
        followup_of(&got, repo, pr, accounts)
            .map_err(|why| format!("the follow-up {repo}#{n} {why}"))?;
        got["html_url"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("GitHub gives no link to {repo}#{n}"))
    }

    /// Whether `pushed` is a new commit on top of `head`, the head the fix
    /// was handed: not that head itself, nor one that replaced it.
    fn pushed_on(&self, repo: &str, head: &str, pushed: &str) -> Result<(), String> {
        let mut api = (self.github)();
        let got = call(
            &mut *api,
            "GET",
            &format!("repos/{repo}/compare/{head}...{pushed}"),
            None,
            None,
        )
        .and_then(|r| r.json())
        .map_err(|e| format!("{head}...{pushed} in {repo} can't be compared: {e}"))?;
        if got["status"] != "ahead" {
            return Err(format!(
                "the fix job's push {pushed} isn't a new commit on {head}: GitHub says it's {}",
                got["status"]
            ));
        }
        Ok(())
    }

    /// The open PR's head and branch, from GitHub.
    fn pr(&self, repo: &str, pr: u64) -> Result<(String, String), String> {
        let mut api = (self.github)();
        let got = call(
            &mut *api,
            "GET",
            &format!("repos/{repo}/pulls/{pr}"),
            None,
            None,
        )
        .and_then(|r| r.json())
        .map_err(|e| format!("{repo}#{pr} can't be read: {e}"))?;
        if got["state"] != "open" {
            return Err(format!("{repo}#{pr} isn't open"));
        }
        let from = got["head"]["repo"]["full_name"]
            .as_str()
            .unwrap_or_default();
        if !from.eq_ignore_ascii_case(repo) {
            return Err(format!(
                "{repo}#{pr}'s branch is in {from:?}, where a fix job can't push"
            ));
        }
        match (got["head"]["sha"].as_str(), got["head"]["ref"].as_str()) {
            (Some(sha), Some(branch)) => Ok((sha.to_owned(), branch.to_owned())),
            _ => Err(format!("GitHub says no head of {repo}#{pr}")),
        }
    }
}
