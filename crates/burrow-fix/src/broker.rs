//! The broker (Conary's `docs/m6-plugins.md`, F): one fix request a
//! connection, from the flow's controller alone (burrow-producer's socket).
//!
//! - It checks the request's head against the PR's on GitHub.
//! - It checks the fixing role would run as the flow chose, on the engine
//!   the plugin names itself by, before anything starts.
//! - It starts the fix as a proj job on the fixing role, under a key of its
//!   own beneath the PR's. At most one live fix runs a PR: one held here,
//!   and none left over from before a restart ([`Broker::reconcile`]).
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
//! The fixes it's waiting on are kept, by their keys, until each ends: a
//! restart stops the jobs whose connections it lost ([`Broker::reconcile`]).
//! A push that already happened stands; the next pass reads the head that
//! moved.
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
    /// The PRs with a live fix here, by [`key`]: a request's own, or the
    /// job key of one that wouldn't stop ([`Held::keep`]), stopped again
    /// before the PR's next fix starts.
    live: Mutex<HashMap<String, Option<String>>>,
}

/// A PR's live fix, here, and the proj key its job is held under: let go
/// when its request ends, however it ends, unless its job may still be at
/// work ([`Held::keep`]).
struct Held<'a> {
    broker: &'a Broker,
    pr: String,
    job: String,
}

impl Held<'_> {
    /// Its job wouldn't stop: the PR stays held by its job's key, kept on
    /// disk too, so no second fix starts beside it until that job is
    /// stopped ([`Broker::hold`], [`Broker::reconcile`]).
    fn keep(self) {
        if let Ok(mut live) = self.broker.live.lock() {
            live.insert(self.pr.clone(), Some(self.job.clone()));
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

    /// At startup: stop each job a fix was waiting on when the broker
    /// stopped, its connection lost with it. How many it stopped. One that
    /// won't stop keeps its PR held, as it would have before the restart,
    /// and the rest are served.
    pub fn reconcile(&self) -> Result<usize, String> {
        let dir = self.cfg.state.join("live");
        let mut stopped = 0;
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let job = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let job = job.trim();
            match self.let_go(job) {
                Ok(found) => {
                    stopped += usize::from(found);
                    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Err(e) => {
                    eprintln!("burrow-fix: {job} wouldn't stop, so its PR stays held: {e}");
                    self.live
                        .lock()
                        .map_err(|_| "the broker's lock is poisoned")?
                        .insert(pr_of(job).to_owned(), Some(job.to_owned()));
                }
            }
        }
        Ok(stopped)
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
        let held = self.hold(key(repo, *pr), &format!("a fix of {repo}#{pr}"))?;
        let branch = self.handed(repo, *pr, head)?;
        let task = brief::task(repo, *pr, head, &branch, problems, &self.cfg.jobs);
        let kept = self.kept(&held.job);
        fs::write(&kept, &held.job).map_err(|e| format!("{}: {e}", kept.display()))?;
        // Stopped before the answer is let go of, so no fix is left at work
        // for a request that's over: a start that read as failed may have
        // started all the same.
        let id = match self.jobs.start(repo, &held.job, &task) {
            Ok(id) => id,
            Err(why) => return Err(self.ended(held, None, why)),
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

    /// The PR's live work under `key` ([`key`], [`stuck_key`]), here, or
    /// why there's `what` at work already.
    fn hold(&self, key: String, what: &str) -> Result<Held<'_>, String> {
        let mut live = self
            .live
            .lock()
            .map_err(|_| "the broker's lock is poisoned")?;
        match live.get(&key) {
            None => {}
            Some(None) => return Err(format!("{what} is at work already")),
            // Work whose job wouldn't stop: stopped now, or still held.
            Some(Some(job)) => {
                self.let_go(job).map_err(|e| {
                    format!("{what} is at work already, and its job {job} won't stop: {e}")
                })?;
                let _ = fs::remove_file(self.kept(job));
            }
        }
        live.insert(key.clone(), None);
        Ok(Held {
            broker: self,
            job: job_key(&key),
            pr: key,
        })
    }

    fn validate_stuck_request<'a>(
        &self,
        request: &'a Request,
    ) -> Result<(&'a str, Ask, &std::path::Path), String> {
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
        let flow = self
            .cfg
            .flow
            .as_deref()
            .ok_or("the broker has no flow configurations to read a repo's authors from")?;
        Ok((repo, ask, flow))
    }

    /// One request for the stuck role: its answer, or why there's none.
    pub fn handle_job(&self, request: Request, work: &Work) -> Result<Response, String> {
        let starter = self
            .stuck
            .as_deref()
            .ok_or("the broker serves no job kind: no stuck role is configured")?;
        let (repo, ask, flow) = self.validate_stuck_request(&request)?;
        let pr = ask.pr;
        let accounts = discussion::accounts(flow, repo)?;
        let held = self.hold(
            stuck_key(repo, pr),
            &format!("a stuck-role job on {repo}#{pr}"),
        )?;
        let (head, branch) = self.pr(repo, pr)?;
        let shown = discussion::read(&mut *(self.github)(), repo, pr, &accounts.discussion)?;
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
        let kept = self.kept(&held.job);
        fs::write(&kept, &held.job).map_err(|e| format!("{}: {e}", kept.display()))?;
        let id = match starter.start(repo, &held.job, &task) {
            Ok(id) => id,
            Err(why) => return Err(self.ended(held, None, why)),
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

    /// The PR's branch, once its head is `head`, it's open, and its branch
    /// is in `repo` itself, where the job can push.
    fn handed(&self, repo: &str, pr: u64, head: &str) -> Result<String, String> {
        let (now, branch) = self.pr(repo, pr)?;
        if now != head {
            return Err(format!(
                "{repo}#{pr}'s head is {now}, not the {head} the fix was asked for"
            ));
        }
        Ok(branch)
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
