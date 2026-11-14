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
//! The fixes it's waiting on are kept, by their keys, until each ends: a
//! restart stops the jobs whose connections it lost ([`Broker::reconcile`]).
//! A push that already happened stands; the next pass reads the head that
//! moved.
use crate::answers::Said;
use crate::jobs::{Jobs, Record};
use crate::{Config, brief};
use burrow_github::{Api, call};
use burrow_producer::serve::Work;
use conary_core::plugin::{
    Answer, Choice, Context, Executed, FixResult, Identity, Request, Response, Task, version,
};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A GitHub to ask, new for each look.
pub type GitHub = Box<dyn Fn() -> Box<dyn Api> + Send + Sync>;

pub struct Broker {
    pub cfg: Config,
    pub jobs: Box<dyn Jobs>,
    pub github: GitHub,
    /// How often it reads a job's record while it waits.
    pub look: Duration,
    /// The PRs with a live fix here, by [`key`].
    live: Mutex<HashSet<String>>,
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
    /// Its job wouldn't stop: the PR stays held, and its job's key kept for
    /// a restart to stop it, so no second fix starts beside it.
    fn keep(self) {
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

/// A PR's own key, in proj's key alphabet (letters, digits and `._/:-`).
/// GitHub's names have no `:`, so an `_` written as one can't make two
/// PRs' keys the same.
pub fn key(repo: &str, pr: u64) -> String {
    format!("fix/{}/{pr}", repo.to_ascii_lowercase().replace('_', ":"))
}

/// The proj key one request's job is held under: the PR's, and a part of
/// its own, since proj never lets go of a key a job that ran holds, and a
/// PR is fixed again and again.
fn job_key(repo: &str, pr: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}/{now:x}", key(repo, pr))
}

impl Broker {
    pub fn new(cfg: Config, jobs: Box<dyn Jobs>, github: GitHub) -> io::Result<Self> {
        fs::create_dir_all(cfg.state.join("live"))?;
        Ok(Self {
            cfg,
            jobs,
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
    /// stopped, its connection lost with it. How many it stopped.
    pub fn reconcile(&self) -> Result<usize, String> {
        let dir = self.cfg.state.join("live");
        let mut stopped = 0;
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let key = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(id) = self.jobs.find(key.trim())? {
                self.jobs.stop(&id)?;
                stopped += 1;
            }
            fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(stopped)
    }

    /// One request's fix: its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
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
        self.runnable(
            repo,
            request.execution.as_ref().and_then(|e| e.choice.as_ref()),
        )?;
        let held = self.hold(repo, *pr)?;
        let branch = self.handed(repo, *pr, head)?;
        let task = brief::task(repo, *pr, head, &branch, problems, &self.cfg.jobs);
        let kept = self.kept(&held.job);
        fs::write(&kept, &held.job).map_err(|e| format!("{}: {e}", kept.display()))?;
        let id = self.jobs.start(repo, &held.job, &task)?;
        let record = match self.wait(&id, work) {
            Ok(record) => record,
            Err(why) => {
                // Stopped before the answer is let go of, so no fix is left
                // at work for a request that's over.
                return Err(match self.jobs.stop(&id) {
                    Ok(()) => why,
                    Err(e) => {
                        held.keep();
                        format!("{why}; and its job {id} wouldn't stop: {e}")
                    }
                });
            }
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

    /// The PR's live fix, here, or why there's one already.
    fn hold(&self, repo: &str, pr: u64) -> Result<Held<'_>, String> {
        let key = key(repo, pr);
        let mut live = self
            .live
            .lock()
            .map_err(|_| "the broker's lock is poisoned")?;
        if !live.insert(key.clone()) {
            return Err(format!("a fix of {repo}#{pr} is at work already"));
        }
        Ok(Held {
            broker: self,
            pr: key,
            job: job_key(repo, pr),
        })
    }

    /// Whether a fix job in `repo` would run as the flow asks, before one
    /// starts: proj runs it on the fixing role's route and tuning, and
    /// Conary refuses an answer on another engine than the plugin names
    /// (the role's route anywhere), a model it didn't offer, or another
    /// model or effort than the flow chose. So the role's route for the
    /// repo is that engine, moves to no other, names its model, and is
    /// what the flow chose.
    fn runnable(&self, repo: &str, choice: Option<&Choice>) -> Result<(), String> {
        let role = &self.cfg.role;
        let settings = self.cfg.settings.role_view()?.in_force;
        let named = &settings.route(role, None).engine;
        let route = settings.route(role, Some(repo));
        if route.engine != *named || route.over.is_some() {
            return Err(format!(
                "the {role} role can run in {repo} on another engine than {named}, which the plugin names itself by"
            ));
        }
        let tuning = settings.tuning(role, Some(repo), named);
        let offers = settings.offers.get(named).cloned().unwrap_or_default();
        if tuning.model.is_none() && !offers.models.is_empty() {
            return Err(format!(
                "the {role} role names no model on {named}, so its answer couldn't say what ran"
            ));
        }
        if let Some(c) = choice
            && (c.engine != *named
                || c.model
                    .as_ref()
                    .is_some_and(|m| Some(m) != tuning.model.as_ref())
                || c.effort
                    .as_ref()
                    .is_some_and(|e| Some(e) != tuning.effort.as_ref()))
        {
            return Err(format!(
                "the flow chose {} {:?} {:?}, and the {role} role in {repo} runs {named} {:?} {:?}",
                c.engine, c.model, c.effort, tuning.model, tuning.effort
            ));
        }
        Ok(())
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

    /// The job's record once its turn has ended done, or why it won't be:
    /// the request ended first, the job went past its own deadline, or its
    /// turn ended any other way.
    fn wait(&self, id: &str, work: &Work) -> Result<Record, String> {
        let mut look = Instant::now();
        loop {
            if work.abandoned() {
                return Err("the request ended before its fix did".into());
            }
            if Instant::now() >= look {
                let record = self.jobs.why(id)?;
                if !record.live() {
                    return if record.done() {
                        Ok(record)
                    } else {
                        Err(format!(
                            "the fix job {id} ended {}, not done: {:?}",
                            record.stage, record.ends
                        ))
                    };
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                if let Some(due) = record.overdue_at
                    && now > due
                {
                    return Err(format!("the fix job {id} is overdue"));
                }
                look = Instant::now() + self.look;
            }
            thread::sleep(self.look.min(Duration::from_millis(100)));
        }
    }
}
