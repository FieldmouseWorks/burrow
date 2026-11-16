//! The runner: the judging engine's side of a judgment, as burrow.user
//! (proj's jobs are that user's), holding no key. Only the producer's user
//! may ask it (burrow-producer's socket, with that user as its
//! controller), and only with a judge request the producer forwards.
//!
//! - It holds the request to GitHub again, through its own reads: the PR
//!   open on the head, with the title and description it was asked on.
//! - It reads the PR's discussion whole, once, and refuses unless that is
//!   what the request's `context` digests: what the job is shown is what
//!   the judgment is sealed on.
//! - It checks the judging role would run as the flow chose, then starts
//!   the judgment as a proj job on that role, briefed with every thread
//!   and answer ([`crate::brief`]), under a key of its own.
//! - It waits on the job through its record, books its deadline from the
//!   record's `overdue_at`, and stops the job when the request ends first.
//! - It answers what the job decided, by the job's own engine and session,
//!   on what ran.
//!
//! The jobs it waits on are kept, by their keys, until each ends: a restart
//! stops the jobs whose connections it lost ([`Runner::reconcile`]).
use crate::discussion::{Discussion, current};
use crate::verdict::Said;
use crate::{Config, Read, brief};
use burrow_producer::jobs::{self, Jobs};
use burrow_producer::serve::Work;
use conary_core::plugin::{Answer, Context, Executed, Identity, Request, Response, Task};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Runner {
    pub cfg: Config,
    pub jobs: Box<dyn Jobs>,
    pub github: Read,
    /// How often it reads a job's record while it waits.
    pub look: Duration,
}

/// The proj key one judgment's job is held under: the PR's, and a part of
/// its own, since proj never lets go of a key a job that ran holds. In
/// proj's key alphabet: GitHub's names have no `:`, so an `_` written as
/// one can't make two PRs' keys the same.
fn job_key(repo: &str, pr: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "judge/{}/{pr}/{now:x}",
        repo.to_ascii_lowercase().replace('_', ":")
    )
}

/// A job a judgment waits on, kept on disk until its request ends, however
/// it ends, unless its job wouldn't stop.
struct Kept {
    path: PathBuf,
    /// Its job wouldn't stop: left for the next start to stop.
    stays: std::cell::Cell<bool>,
}

impl Drop for Kept {
    fn drop(&mut self) {
        if !self.stays.get() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Runner {
    pub fn new(cfg: Config, jobs: Box<dyn Jobs>, github: Read) -> io::Result<Self> {
        fs::create_dir_all(cfg.runner_state.join("live"))?;
        Ok(Self {
            cfg,
            jobs,
            github,
            look: Duration::from_secs(10),
        })
    }

    fn kept(&self, key: &str) -> PathBuf {
        self.cfg
            .runner_state
            .join("live")
            .join(key.replace('/', "+"))
    }

    /// At startup: stop each job a judgment was waiting on when the runner
    /// stopped, its connection lost with it. How many it stopped. One that
    /// won't stop is kept for the next start, and the rest are served.
    pub fn reconcile(&self) -> Result<usize, String> {
        let dir = self.cfg.runner_state.join("live");
        let mut stopped = 0;
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let key = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let found = self.jobs.find(key.trim());
            match found.and_then(|id| id.map(|id| self.jobs.stop(&id)).transpose()) {
                Ok(was) => {
                    stopped += usize::from(was.is_some());
                    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Err(e) => eprintln!("burrow-judge: {} wouldn't stop: {e}", key.trim()),
            }
        }
        Ok(stopped)
    }

    /// One judgment: its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
        crate::held(&request)?;
        let Task::Judge {
            repo,
            pr,
            head,
            against,
            why,
            context,
            ..
        } = &request.task
        else {
            return Err("the judge judges alone".into());
        };
        jobs::runnable(
            &self.cfg.settings,
            &self.cfg.role,
            repo,
            &jobs::offered(&self.cfg.settings, &self.cfg.role)?,
            request.execution.as_ref().and_then(|e| e.choice.as_ref()),
        )?;
        let source = (self.github)()?;
        let (pull, _) = current(&*source, repo, *pr, head, against)?;
        let discussion = Discussion::read(&*source, repo, *pr)?;
        if discussion.context() != *context {
            return Err(format!(
                "{repo}#{pr}'s discussion isn't the one the judgment was asked on"
            ));
        }
        drop(source);
        let task = brief::task(&brief::Judge {
            repo,
            pr: *pr,
            head,
            base: &pull.base.name,
            title: &pull.title,
            body: pull.body.as_deref().unwrap_or(""),
            why,
            owner: &self.cfg.owner,
            discussion: &discussion,
            jobs: &self.cfg.jobs,
        });

        let key = job_key(repo, *pr);
        let kept = Kept {
            path: self.kept(&key),
            stays: Default::default(),
        };
        fs::write(&kept.path, &key).map_err(|e| format!("{}: {e}", kept.path.display()))?;
        // Stopped before the answer is let go of, so no judgment is left at
        // work for a request that's over: a start that read as failed may
        // have started all the same.
        let ended = |id: Option<&str>, why: String| -> String {
            let stopped = match id {
                Some(id) => self.jobs.stop(id),
                None => self
                    .jobs
                    .find(&key)
                    .and_then(|id| id.map_or(Ok(()), |id| self.jobs.stop(&id))),
            };
            match stopped {
                Ok(()) => why,
                Err(e) => {
                    kept.stays.set(true);
                    format!("{why}; and its job wouldn't stop: {e}")
                }
            }
        };
        let id = match self.jobs.start(repo, &key, &task) {
            Ok(id) => id,
            Err(why) => return Err(ended(None, why)),
        };
        let record = match jobs::wait(&*self.jobs, &id, work, self.look) {
            Ok(record) => record,
            Err(why) => return Err(ended(Some(&id), why)),
        };
        let said = self
            .jobs
            .answers(&id)?
            .ok_or_else(|| format!("the judge job {id} left no answers"))?;
        let (verdict, reasons) = Said::read(&said)?;
        let turn = record
            .turn
            .ok_or_else(|| format!("the judge job {id} ran no turn"))?;
        let engine = turn
            .engine
            .ok_or_else(|| format!("the judge job {id}'s turn names no engine"))?;
        let session = turn
            .session
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("the judge job {id}'s turn names no session"))?;
        Ok(Response {
            contract: request.contract,
            by: Identity {
                engine,
                session: Some(session),
            },
            execution: Some(Executed {
                model: turn.model,
                effort: turn.effort,
                context: Context::Fresh,
            }),
            answer: Answer::Judge {
                verdict,
                why: reasons,
            },
        })
    }
}
