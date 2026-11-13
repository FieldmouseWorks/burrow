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
const UNIT: &str = "org.freedesktop.systemd1.Unit";

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
    fn unit_path(&self) -> &str {
        &self.5
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

/// The jobs to let go: waiting jobs of the activation's own work (a unit it
/// lists, or a job that wasn't queued before it began) that wait, directly
/// or through other waiting jobs, on nothing but jobs of `running`, the ones
/// that were running as it began; each with a run it waits on. A job that
/// also waits, on any branch, for a job of anything else (a new unit that
/// is starting, say) is kept whole, so that wait still counts and still
/// reverts a start that hangs. `waits_for` says what a job waits for, as
/// systemd does.
pub fn held<'j>(
    jobs: &'j [Job],
    touched: &HashSet<String>,
    existing: &HashSet<u32>,
    running: &HashSet<u32>,
    mut waits_for: impl FnMut(&Job) -> Vec<Job>,
) -> Vec<(&'j Job, Job)> {
    // For each job, the run it ends up waiting on if all it waits for is
    // waiting jobs that do, or such a run: `None` if anything else.
    let mut memo: HashMap<u32, Option<Job>> = HashMap::new();
    let mut held = Vec::new();
    for job in jobs {
        let ours = touched.contains(job.unit()) || !existing.contains(&job.id());
        if !job.waiting() || !ours || queued_by(job.kind()).is_none() {
            continue;
        }
        if let Some(run) = only_behind(job, running, &mut memo, &mut waits_for, &mut HashSet::new())
        {
            held.push((job, run));
        }
    }
    held
}

fn only_behind(
    job: &Job,
    running: &HashSet<u32>,
    memo: &mut HashMap<u32, Option<Job>>,
    waits_for: &mut impl FnMut(&Job) -> Vec<Job>,
    path: &mut HashSet<u32>,
) -> Option<Job> {
    if let Some(known) = memo.get(&job.id()) {
        return known.clone();
    }
    // A wait that goes round holds on to nothing new.
    if !path.insert(job.id()) {
        return None;
    }
    let ahead = waits_for(job);
    let mut run = None;
    let mut pure = !ahead.is_empty();
    for other in &ahead {
        if other.running() && running.contains(&other.id()) {
            run.get_or_insert_with(|| other.clone());
        } else if other.waiting() {
            match only_behind(other, running, memo, waits_for, path) {
                Some(r) => {
                    run.get_or_insert(r);
                }
                None => pure = false,
            }
        } else {
            pure = false;
        }
    }
    path.remove(&job.id());
    let result = run.filter(|_| pure);
    memo.insert(job.id(), result.clone());
    result
}

/// Whether every job in `after`, and every one that waits on those, is a
/// target's: nothing that runs of its own, so one let go for a while holds
/// back nothing that must follow it.
pub fn only_targets_follow(job: &Job, mut after: impl FnMut(&Job) -> Vec<Job>) -> bool {
    let mut seen = HashSet::from([job.id()]);
    let mut next = vec![job.clone()];
    while let Some(j) = next.pop() {
        for o in after(&j) {
            if !seen.insert(o.id()) {
                continue;
            }
            if !o.unit().ends_with(".target") {
                return false;
            }
            next.push(o);
        }
    }
    true
}

/// `c` run, as an activation, with its jobs kept from waiting behind a run
/// that was going before it started: how it exited. A job it let go and
/// couldn't queue again is an error: that start would be lost.
pub fn activate(cfg: &Config, c: &mut Command) -> io::Result<ExitStatus> {
    let mut queue = Queue::now(cfg);
    let mut child: Child = c.spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return match queue.lost.as_slice() {
                [] => Ok(status),
                lost => {
                    let text = format!(
                        "{} was let go and could not be queued again: its start would be lost",
                        lost.join(", ")
                    );
                    complain(&text);
                    cfg.journal("err", &text);
                    Err(io::Error::other(text))
                }
            };
        }
        queue.look();
        thread::sleep(LOOK);
    }
}

/// systemd's queue as an activation began.
struct Queue<'c> {
    cfg: &'c Config,
    /// Every job there was, and the ones that were running.
    existing: HashSet<u32>,
    running: HashSet<u32>,
    /// Units whose job was let go and not queued again.
    lost: Vec<String>,
    /// Units already said to be let go.
    said: HashSet<String>,
}

impl<'c> Queue<'c> {
    fn now(cfg: &'c Config) -> Queue<'c> {
        let jobs = Queue::jobs_of(cfg).unwrap_or_default();
        Queue {
            cfg,
            existing: jobs.iter().map(Job::id).collect(),
            running: jobs.iter().filter(|j| j.running()).map(Job::id).collect(),
            lost: Vec::new(),
            said: HashSet::new(),
        }
    }

    /// One look: what's held is let go and queued again. A queue it can't
    /// read is left as it is.
    fn look(&mut self) {
        if self.running.is_empty() {
            return;
        }
        let Some(jobs) = Queue::jobs_of(self.cfg) else {
            return;
        };
        let touched = self.touched();
        let cfg = self.cfg;
        let held = held(&jobs, &touched, &self.existing, &self.running, |j| {
            call(cfg, &[j.path(), JOB, "GetBefore"]).unwrap_or_default()
        });
        for (job, run) in held {
            self.release(&jobs, job, &run);
        }
    }

    /// One held job, let go and queued again, unless something would be
    /// lost by it or it isn't waiting any more.
    fn release(&mut self, jobs: &[Job], job: &Job, run: &Job) {
        let cfg = self.cfg;
        let Some(method) = queued_by(job.kind()) else {
            return;
        };
        // What requires it, or must follow it, would fail or run early.
        if self.required(jobs, job)
            || !only_targets_follow(job, |j| {
                call(cfg, &[j.path(), JOB, "GetAfter"]).unwrap_or_default()
            })
        {
            return;
        }
        // Looked at again right before: the run may have ended meanwhile
        // and the job started, which Cancel would let go all the same.
        let state = |j: &Job| property::<String>(cfg, j.path(), JOB, "State");
        if state(job).as_deref() != Some("waiting") || state(run).as_deref() != Some("running") {
            return;
        }
        // A job that ended meanwhile is left be.
        if call::<()>(cfg, &[job.path(), JOB, "Cancel"]).is_none() {
            return;
        }
        let queued = (0..3).any(|_| {
            call::<String>(
                cfg,
                &[SYSTEMD[1], MANAGER, method, "ss", job.unit(), "replace"],
            )
            .as_deref()
            .and_then(job_id)
            .is_some()
        });
        if !queued {
            let line = format!(
                "{} {} was let go and could not be queued again",
                job.unit(),
                job.kind()
            );
            complain(&line);
            cfg.journal("err", &line);
            self.lost.push(job.unit().to_string());
            return;
        }
        if self.said.insert(job.unit().to_string()) {
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

    /// Whether a unit that requires the job's, or is bound to it, has a job
    /// of its own: it would fail with this one's cancelling.
    fn required(&self, jobs: &[Job], job: &Job) -> bool {
        let queued: HashSet<&str> = jobs.iter().map(Job::unit).collect();
        let unit = job.unit_path();
        ["RequiredBy", "BoundBy", "RequisiteOf"].iter().any(|p| {
            property::<Vec<String>>(self.cfg, unit, UNIT, p)
                .is_none_or(|by| by.iter().any(|u| queued.contains(u.as_str())))
        })
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
    let out = busctl(cfg, "call", words)?;
    reply(&out)
}

/// An object's property, as busctl says it: `{"type":..,"data":VALUE}`.
fn property<T: for<'de> Deserialize<'de>>(
    cfg: &Config,
    path: &str,
    iface: &str,
    name: &str,
) -> Option<T> {
    #[derive(Deserialize)]
    struct Value<T> {
        data: T,
    }
    let out = busctl(cfg, "get-property", &[path, iface, name])?;
    serde_json::from_slice::<Value<T>>(&out)
        .ok()
        .map(|v| v.data)
}

fn busctl(cfg: &Config, verb: &str, words: &[&str]) -> Option<Vec<u8>> {
    Command::new(&cfg.tools.busctl)
        .args(["--json=short", verb, SYSTEMD[0]])
        .args(words)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
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

    /// What `held` finds, as (held unit, run's unit) pairs. `existing` are
    /// the ids there were as the activation began, `running` the running ones.
    fn found(
        jobs: &[Job],
        touched: &[&str],
        existing: &[u32],
        running: &[u32],
        waits: &[(u32, &[u32])],
    ) -> Vec<(String, String)> {
        let by_id: HashMap<u32, Job> = jobs.iter().map(|j| (j.id(), j.clone())).collect();
        let waits: HashMap<u32, Vec<u32>> = waits.iter().map(|(k, v)| (*k, v.to_vec())).collect();
        let touched = touched.iter().map(|t| t.to_string()).collect();
        held(jobs, &touched, &set(existing), &set(running), |j| {
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

    fn sweep_behind_a_run() -> [Job; 2] {
        [
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
        ]
    }

    #[test]
    fn a_restart_behind_a_run_from_before_the_activation_is_held() {
        assert_eq!(
            found(
                &sweep_behind_a_run(),
                &["renovate-sweep.service"],
                &[1],
                &[1],
                &[(9, &[1])]
            ),
            [("renovate-sweep.service".into(), "renovate.service".into())]
        );
    }

    #[test]
    fn a_target_waiting_only_behind_the_run_is_let_go_with_what_it_waits_on() {
        // multi-user.target waits on a sweep queued before the switch,
        // which waits on the run.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(2, "renovate-sweep.service", "start", "waiting"),
            job(9, "multi-user.target", "start", "waiting"),
        ];
        let mut got = found(
            &jobs,
            &["multi-user.target", "renovate-sweep.service"],
            &[1, 2],
            &[1],
            &[(9, &[2]), (2, &[1])],
        );
        got.sort();
        assert_eq!(
            got,
            [
                ("multi-user.target".into(), "renovate.service".into()),
                ("renovate-sweep.service".into(), "renovate.service".into())
            ]
        );
    }

    #[test]
    fn a_job_the_activation_queued_behind_the_run_is_let_go_though_not_listed() {
        // `pass` came with the target: not listed, a job of this activation.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(10, "pass.service", "start", "waiting"),
        ];
        assert_eq!(
            found(&jobs, &["multi-user.target"], &[1], &[1], &[(10, &[1])]),
            [("pass.service".into(), "renovate.service".into())]
        );
        // One that was queued before, and isn't listed, is someone else's.
        assert!(found(&jobs, &["multi-user.target"], &[1, 10], &[1], &[(10, &[1])]).is_empty());
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
        assert!(found(&jobs, &["multi-user.target"], &[1], &[1], &[(11, &[10])]).is_empty());
    }

    #[test]
    fn a_job_that_also_waits_on_something_else_is_kept_whole() {
        // The target waits on the sweep behind the run and on a new unit
        // that hangs: the target keeps both waits, the sweep alone goes.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(2, "sweep.service", "start", "waiting"),
            job(10, "hung.service", "start", "running"),
            job(11, "multi-user.target", "start", "waiting"),
        ];
        assert_eq!(
            found(
                &jobs,
                &["multi-user.target", "sweep.service"],
                &[1, 2],
                &[1],
                &[(11, &[2, 10]), (2, &[1])]
            ),
            [("sweep.service".into(), "renovate.service".into())]
        );
        // Through an intermediate waiting job, too, and when the other
        // branch is a job with nothing it could be waiting for.
        let jobs = [
            job(1, "renovate.service", "start", "running"),
            job(2, "sweep.service", "start", "waiting"),
            job(3, "mid.target", "start", "waiting"),
            job(10, "late.service", "start", "waiting"),
            job(11, "multi-user.target", "start", "waiting"),
        ];
        let got = found(
            &jobs,
            &["multi-user.target", "sweep.service"],
            &[1, 2, 3],
            &[1],
            &[(11, &[3, 10]), (3, &[2]), (2, &[1])],
        );
        assert_eq!(got, [("sweep.service".into(), "renovate.service".into())]);
    }

    #[test]
    fn a_waiting_job_that_started_later_is_not_a_run_from_before() {
        // Job 2 was queued before the activation and waiting; it runs now
        // with the new configuration. It isn't a run to let go behind.
        let jobs = [
            job(2, "slow.service", "start", "running"),
            job(9, "app.service", "restart", "waiting"),
        ];
        assert!(found(&jobs, &["app.service"], &[2, 9], &[], &[(9, &[2])]).is_empty());
    }

    #[test]
    fn only_the_activations_own_work_and_kinds_are_let_go() {
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
                &[1, 2, 3, 4],
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
        assert!(found(&jobs, &["a.service"], &[5, 6], &[], &[(5, &[6]), (6, &[5])]).is_empty());
    }

    #[test]
    fn only_targets_may_follow_a_job_let_go() {
        let sweep = job(2, "sweep.service", "start", "waiting");
        let all = [
            job(3, "multi-user.target", "start", "waiting"),
            job(4, "consumer.service", "start", "waiting"),
            job(5, "late.target", "start", "waiting"),
        ];
        let follow = |ids: &'static [(u32, &'static [usize])]| {
            let all = all.clone();
            move |j: &Job| {
                ids.iter()
                    .filter(|(k, _)| *k == j.id())
                    .flat_map(|(_, v)| v.iter().map(|i| all[*i].clone()))
                    .collect::<Vec<_>>()
            }
        };
        assert!(only_targets_follow(&sweep, follow(&[(2, &[0])])));
        assert!(only_targets_follow(&sweep, follow(&[])));
        // A service ordered after it would run before it, in the gap.
        assert!(!only_targets_follow(&sweep, follow(&[(2, &[0, 1])])));
        // Through a target to a service.
        assert!(!only_targets_follow(
            &sweep,
            follow(&[(2, &[2]), (5, &[1])])
        ));
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
