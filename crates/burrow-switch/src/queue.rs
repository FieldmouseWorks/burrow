//! An activation kept from waiting behind a run that was already going
//! (burrow#811).
//!
//! switch-to-configuration waits on every job it queues: the units it
//! starts, restarts and reloads, which it lists in /run/nixos while it
//! works. systemd holds a job back while a job it is ordered after hasn't
//! ended, so a unit it restarts that is ordered after a long run
//! (renovate-sweep, after renovate, which goes on for up to two hours)
//! holds the activation until that run ends: past its deadline, and the
//! switch reverts with nothing wrong in it.
//!
//! So while an activation runs, whatever runs it looks at systemd's own
//! queue every second. A job of a listed unit that waits, directly or
//! through other waiting jobs, on a job that was already running when the
//! activation began is cancelled and queued again, with nothing waiting
//! for it: the unit still starts after that run, as it would have, and the
//! activation goes on. systemd says which job waits on which
//! (org.freedesktop.systemd1.Job.GetBefore), so nothing here models its
//! ordering, and units from packages count as much as the flake's own.
//! What the activation started itself is still waited for: a start that
//! hangs still reverts the switch.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::{Config, complain};

/// How often it looks.
const LOOK: Duration = Duration::from_secs(1);

/// The lists switch-to-configuration keeps, in `Config::activation_lists`,
/// of the units it starts, restarts and reloads, until it has.
const LISTS: [&str; 3] = ["start-list", "restart-list", "reload-list"];

/// What it restarts besides, and waits for, without listing it.
const REACTIVATION: &str = "sysinit-reactivation.target";

const SYSTEMD: [&str; 2] = ["org.freedesktop.systemd1", "/org/freedesktop/systemd1"];
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const JOB: &str = "org.freedesktop.systemd1.Job";

/// A job, as systemd lists one: its id, unit, type and state, its object
/// and its unit's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Job(
    pub u32,
    pub String,
    pub String,
    pub String,
    pub String,
    pub String,
);

impl Job {
    fn id(&self) -> u32 {
        self.0
    }
    fn unit(&self) -> &str {
        &self.1
    }
    fn kind(&self) -> &str {
        &self.2
    }
    fn running(&self) -> bool {
        self.3 == "running"
    }
    fn waiting(&self) -> bool {
        self.3 == "waiting"
    }
    fn path(&self) -> &str {
        &self.4
    }
}

/// The Manager's method that queues a job of `kind` again: none for a kind
/// the activation doesn't queue.
fn queued_by(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "start" => "StartUnit",
        "restart" => "RestartUnit",
        "reload" => "ReloadUnit",
        "try-restart" => "TryRestartUnit",
        _ => return None,
    })
}

/// The jobs of `touched` units that wait, directly or through other waiting
/// jobs, on a job of `before` that is running: each with that run.
/// `waits_for` says what a job waits for, as systemd does.
pub fn held<'j>(
    jobs: &'j [Job],
    touched: &HashSet<String>,
    before: &HashSet<u32>,
    mut waits_for: impl FnMut(&Job) -> Vec<Job>,
) -> Vec<(&'j Job, Job)> {
    let mut known: HashMap<u32, Vec<Job>> = HashMap::new();
    let mut held = Vec::new();
    for job in jobs {
        if !job.waiting() || !touched.contains(job.unit()) || queued_by(job.kind()).is_none() {
            continue;
        }
        let mut seen = HashSet::from([job.id()]);
        let mut next = vec![job.clone()];
        let run = 'search: loop {
            let Some(j) = next.pop() else { break None };
            let ahead = known.entry(j.id()).or_insert_with(|| waits_for(&j)).clone();
            for other in ahead {
                if other.running() && before.contains(&other.id()) {
                    break 'search Some(other);
                }
                if other.waiting() && seen.insert(other.id()) {
                    next.push(other);
                }
            }
        };
        if let Some(run) = run {
            held.push((job, run));
        }
    }
    held
}

/// `c` run, as an activation, with its jobs kept from waiting behind a run
/// that was going before it started: how it exited.
pub fn activate(cfg: &Config, c: &mut Command) -> io::Result<ExitStatus> {
    let mut queue = Queue::now(cfg);
    let mut child: Child = c.spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        queue.look();
        thread::sleep(LOOK);
    }
}

/// systemd's queue as an activation began, and the jobs it queued again
/// since.
struct Queue<'c> {
    cfg: &'c Config,
    before: HashSet<u32>,
    ours: HashSet<u32>,
}

impl<'c> Queue<'c> {
    fn now(cfg: &'c Config) -> Queue<'c> {
        let before = Queue::jobs_of(cfg)
            .unwrap_or_default()
            .iter()
            .map(Job::id)
            .collect();
        Queue {
            cfg,
            before,
            ours: HashSet::new(),
        }
    }

    /// One look: what's held is let go and queued again. A queue it can't
    /// read is left as it is.
    fn look(&mut self) {
        if self.before.is_empty() {
            return;
        }
        let Some(jobs) = Queue::jobs_of(self.cfg) else {
            return;
        };
        let jobs: Vec<Job> = jobs
            .into_iter()
            .filter(|j| !self.ours.contains(&j.id()))
            .collect();
        let touched = self.touched();
        let cfg = self.cfg;
        let held = held(&jobs, &touched, &self.before, |j| {
            call(cfg, &[j.path(), JOB, "GetBefore"]).unwrap_or_default()
        });
        for (job, run) in held {
            // A job that ended meanwhile is left be.
            if call::<()>(cfg, &[job.path(), JOB, "Cancel"]).is_none() {
                continue;
            }
            let Some(method) = queued_by(job.kind()) else {
                continue;
            };
            let again: Option<String> = call(
                cfg,
                &[SYSTEMD[1], MANAGER, method, "ss", job.unit(), "replace"],
            );
            if let Some(id) = again.as_deref().and_then(job_id) {
                self.ours.insert(id);
            }
            let line = format!(
                "{} {} waited behind {} (job {}, running since before the activation): queued again, not waited for",
                job.unit(),
                job.kind(),
                run.unit(),
                run.id()
            );
            complain(&line);
            cfg.journal("warning", &line);
        }
    }

    /// The units the activation queues jobs for and waits on.
    fn touched(&self) -> HashSet<String> {
        let dir: &Path = &self.cfg.activation_lists;
        LISTS
            .iter()
            .filter_map(|l| fs::read_to_string(dir.join(l)).ok())
            .flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
            .filter(|l| !l.is_empty())
            .chain([REACTIVATION.to_string()])
            .collect()
    }

    fn jobs_of(cfg: &Config) -> Option<Vec<Job>> {
        call(cfg, &[SYSTEMD[1], MANAGER, "ListJobs"])
    }
}

/// A job's id, from its object's path.
fn job_id(path: &str) -> Option<u32> {
    path.strip_prefix("/org/freedesktop/systemd1/job/")?
        .parse()
        .ok()
}

/// busctl's reply to a call on systemd: its one value, or nothing when it
/// fails or says something else. A call that returns nothing reads as `()`.
fn call<T: for<'de> Deserialize<'de>>(cfg: &Config, words: &[&str]) -> Option<T> {
    let out = Command::new(&cfg.tools.busctl)
        .args(["--json=short", "call", SYSTEMD[0]])
        .args(words)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    reply(&out.stdout)
}

/// What busctl --json=short says a call returned: `{"type":..,"data":[..]}`,
/// its values in `data`; nothing at all for a call that returns none.
fn reply<T: for<'de> Deserialize<'de>>(out: &[u8]) -> Option<T> {
    #[derive(Deserialize)]
    struct Reply<T> {
        data: (T,),
    }
    let text = String::from_utf8_lossy(out);
    if text.trim().is_empty() {
        return serde_json::from_str("null").ok();
    }
    serde_json::from_str::<Reply<T>>(&text)
        .ok()
        .map(|r| r.data.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u32, unit: &str, kind: &str, state: &str) -> Job {
        Job(
            id,
            unit.into(),
            kind.into(),
            state.into(),
            format!("/org/freedesktop/systemd1/job/{id}"),
            String::new(),
        )
    }

    fn set<T: Clone + Eq + std::hash::Hash>(items: &[T]) -> HashSet<T> {
        items.iter().cloned().collect()
    }

    /// What `held` finds, as (held unit, run's unit) pairs.
    fn found(
        jobs: &[Job],
        touched: &[&str],
        before: &[u32],
        waits: &[(u32, &[u32])],
    ) -> Vec<(String, String)> {
        let by_id: HashMap<u32, Job> = jobs.iter().map(|j| (j.id(), j.clone())).collect();
        let waits: HashMap<u32, Vec<u32>> = waits.iter().map(|(k, v)| (*k, v.to_vec())).collect();
        let touched = touched.iter().map(|t| t.to_string()).collect();
        held(jobs, &touched, &set(before), |j| {
            waits
                .get(&j.id())
                .into_iter()
                .flatten()
                .map(|id| by_id[id].clone())
                .collect()
        })
        .into_iter()
        .map(|(j, r)| (j.unit().to_string(), r.unit().to_string()))
        .collect()
    }

    #[test]
    fn a_restart_behind_a_run_from_before_the_activation_is_held() {
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
        ];
        assert_eq!(
            found(&jobs, &["renovate-sweep.service"], &[1], &[(9, &[1])]),
            [("renovate-sweep.service".into(), "renovate.service".into())]
        );
    }

    #[test]
    fn so_is_one_behind_it_through_other_waiting_jobs_or_queued_before() {
        // multi-user.target, started as every active target is, waits on
        // a sweep queued before the switch, which waits on the run.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(2, "renovate-sweep.service", "start", "waiting"),
            job(9, "multi-user.target", "start", "waiting"),
        ];
        assert_eq!(
            found(
                &jobs,
                &["multi-user.target"],
                &[1, 2],
                &[(9, &[2]), (2, &[1])]
            ),
            [("multi-user.target".into(), "renovate.service".into())]
        );
    }

    #[test]
    fn what_the_activation_started_itself_is_still_waited_for() {
        // A new unit the target pulled in that hangs starting: the
        // switch reverts, as it should.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(10, "hung.service", "start", "running"),
            job(11, "multi-user.target", "start", "waiting"),
        ];
        assert!(found(&jobs, &["multi-user.target"], &[1], &[(11, &[10])]).is_empty());
    }

    #[test]
    fn only_the_activations_own_units_and_kinds_are_let_go() {
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            // Someone else's, queued behind it.
            job(2, "other.service", "start", "waiting"),
            // Not a kind the activation queues.
            job(3, "sweep.service", "verify-active", "waiting"),
            // Running, not waiting.
            job(4, "app.service", "start", "running"),
        ];
        assert!(
            found(
                &jobs,
                &["sweep.service", "app.service"],
                &[1, 4],
                &[(2, &[1]), (3, &[1])]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_wait_that_goes_round_ends() {
        let jobs = [
            job(5, "a.service", "start", "waiting"),
            job(6, "b.service", "start", "waiting"),
        ];
        assert!(found(&jobs, &["a.service"], &[5, 6], &[(5, &[6]), (6, &[5])]).is_empty());
    }

    #[test]
    fn busctl_says_a_call_returned_its_one_value() {
        let jobs: Option<Vec<Job>> = reply(
            br#"{"type":"a(usssoo)","data":[[[7,"a.service","start","waiting","/org/freedesktop/systemd1/job/7","/u"]]]}"#,
        );
        assert_eq!(
            jobs,
            Some(vec![job(7, "a.service", "start", "waiting")]).map(|mut v| {
                v[0].5 = "/u".into();
                v
            })
        );
        let none: Option<Vec<Job>> = reply(br#"{"type":"a(usssoo)","data":[[]]}"#);
        assert_eq!(none, Some(vec![]));
        assert_eq!(reply::<()>(b""), Some(()));
        assert_eq!(reply::<Vec<Job>>(b"nonsense"), None);
        let path: Option<String> =
            reply(br#"{"type":"o","data":["/org/freedesktop/systemd1/job/42"]}"#);
        assert_eq!(path.as_deref().and_then(job_id), Some(42));
        assert_eq!(job_id("/org/freedesktop/systemd1/unit/x"), None);
    }
}
