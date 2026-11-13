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
//! queue every second. A job the activation waits on (one of a unit it
//! lists, or a new one that such a job waits for) that waits, directly or
//! through other waiting jobs, on nothing but starts that were already
//! running when the activation began is cancelled and queued again, with
//! nothing waiting for it: the unit still starts after that run, as it
//! would have, and the activation goes on. systemd says which job waits on
//! which (org.freedesktop.systemd1.Job.GetBefore), so nothing here models
//! its ordering, and units from packages count as much as the flake's own.
//! What the activation started itself is still waited for: a start that
//! hangs still reverts the switch. A job queued again that turns out not to
//! wait behind the run after all is waited for here, as the activation
//! would have.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::{Config, complain};

/// How often it looks.
const LOOK: Duration = Duration::from_secs(1);

/// The lists switch-to-configuration keeps, in `Config::activation_lists`,
/// of the units it starts, restarts and reloads, until it has.
const LISTS: [&str; 3] = ["start-list", "restart-list", "reload-list"];

/// The list switch-to-configuration reads back as it starts, the revert's
/// included, of units to start (or restart) whatever else it does. It reads
/// and removes it before it queues any job, so what goes in while it waits
/// on its jobs is the next activation's.
const RECOVERY: &str = "activation-restart-list";

/// What it restarts besides, and waits for, without listing it.
const REACTIVATION: &str = "sysinit-reactivation.target";

/// How a job is queued again: ordered as before, but without pulling in what
/// its unit requires again. Its first queuing did that, and doing it again
/// would run a oneshot it pulls in that has already finished once more.
const MODE: &str = "ignore-requirements";

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

/// A unit's invocation, as systemd says it, if it names one: an empty or
/// all-zero id is none, and none is never the same run as another.
pub fn known(invocation: Option<Vec<u8>>) -> Option<Vec<u8>> {
    invocation.filter(|i| i.iter().any(|b| *b != 0))
}

/// Whether a job running now is the run `then` was, as the activation
/// began: a start then and now, of the same known invocation. Only a start
/// counts. A start merged with anything but a start changes its kind, and
/// one merged with a start is the same startup; a reload merged into a
/// running reload keeps its id, kind and invocation, so a later reload
/// can't be told from the first.
pub fn same_run(then: Option<&Vec<u8>>, kind: &str, now: Option<Vec<u8>>) -> bool {
    kind == "start" && then.is_some() && then == known(now).as_ref()
}

/// The jobs to let go: waiting jobs the activation waits on that wait,
/// directly or through other waiting jobs, on nothing but jobs of
/// `running`, the runs that were going as it began; each with a run it
/// waits on. The activation waits on a job of a unit it lists, and on a job
/// queued since it began that one of those waits for: a job of anything
/// else is someone else's, however new, and is left be. A job that also
/// waits, on any branch, for a job of anything else (a new unit that is
/// starting, say) is kept whole, so that wait still counts and still
/// reverts a start that hangs. `waits_for` says what a job waits for, as
/// systemd does.
pub fn held<'j>(
    jobs: &'j [Job],
    touched: &HashSet<String>,
    existing: &HashSet<u32>,
    running: &HashSet<u32>,
    waits_for: impl FnMut(&Job) -> Vec<Job>,
) -> Vec<(&'j Job, Job)> {
    let mut graph = Graph::new(running, waits_for);
    let reached = graph.reached(jobs, touched);
    let mut held = Vec::new();
    for job in jobs {
        let ours = touched.contains(job.unit())
            || (reached.contains(&job.id()) && !existing.contains(&job.id()));
        if !job.waiting() || !ours || queued_by(job.kind()).is_none() {
            continue;
        }
        if let Some(run) = graph.behind(job) {
            held.push((job, run));
        }
    }
    held
}

/// What waits on what, as systemd says it, each job asked once.
pub struct Graph<'r, F> {
    running: &'r HashSet<u32>,
    waits_for: F,
    before: HashMap<u32, Vec<Job>>,
    /// For each job, the run it ends up waiting on if all it waits for is
    /// waiting jobs that do, or such a run: `None` if anything else.
    memo: HashMap<u32, Option<Job>>,
}

impl<'r, F: FnMut(&Job) -> Vec<Job>> Graph<'r, F> {
    pub fn new(running: &'r HashSet<u32>, waits_for: F) -> Self {
        Graph {
            running,
            waits_for,
            before: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    fn ahead(&mut self, job: &Job) -> Vec<Job> {
        if let Some(known) = self.before.get(&job.id()) {
            return known.clone();
        }
        let ahead = (self.waits_for)(job);
        self.before.insert(job.id(), ahead.clone());
        ahead
    }

    /// The jobs of `touched` units, and the waiting jobs they wait for,
    /// through waiting jobs.
    fn reached(&mut self, jobs: &[Job], touched: &HashSet<String>) -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut next: Vec<Job> = jobs
            .iter()
            .filter(|j| touched.contains(j.unit()))
            .cloned()
            .collect();
        while let Some(j) = next.pop() {
            if !seen.insert(j.id()) || !j.waiting() {
                continue;
            }
            next.extend(self.ahead(&j).into_iter().filter(Job::waiting));
        }
        seen
    }

    /// The run `job` waits behind, if it is waiting and all it waits for,
    /// on every branch, is such runs.
    pub fn behind(&mut self, job: &Job) -> Option<Job> {
        if !job.waiting() {
            return None;
        }
        self.only_behind(job, &mut HashSet::new())
    }

    fn only_behind(&mut self, job: &Job, path: &mut HashSet<u32>) -> Option<Job> {
        if let Some(known) = self.memo.get(&job.id()) {
            return known.clone();
        }
        // A wait that goes round holds on to nothing new.
        if !path.insert(job.id()) {
            return None;
        }
        let ahead = self.ahead(job);
        let mut run = None;
        let mut pure = !ahead.is_empty();
        for other in &ahead {
            if other.running() && self.running.contains(&other.id()) {
                run.get_or_insert_with(|| other.clone());
            } else if other.waiting() {
                match self.only_behind(other, path) {
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
        self.memo.insert(job.id(), result.clone());
        result
    }
}

/// Whether every job in `after`, and every one that waits on those, is a
/// target's: nothing that runs of its own, so one let go for a while holds
/// back nothing that must follow it.
/// A follower list that can't be read counts as a follower: it fails closed.
pub fn only_targets_follow(job: &Job, mut after: impl FnMut(&Job) -> Option<Vec<Job>>) -> bool {
    let mut seen = HashSet::from([job.id()]);
    let mut next = vec![job.clone()];
    while let Some(j) = next.pop() {
        let Some(followers) = after(&j) else {
            return false;
        };
        for o in followers {
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
/// that was going before it started: how it exited. A job queued again
/// that doesn't wait behind the run after all is waited for once it has
/// exited, until `deadline`. A job it let go and couldn't queue again, or
/// one it waited for that failed or outlasted the deadline, is an error, as
/// it would have been for the activation.
pub fn activate(
    cfg: &Config,
    c: &mut Command,
    deadline: Option<Instant>,
) -> io::Result<ExitStatus> {
    let mut queue = Queue::now(cfg);
    let mut child: Child = c.spawn()?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        queue.look(true);
        thread::sleep(LOOK);
    };
    while !queue.tracked.is_empty() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let late = queue.tracked.drain(..).map(|t| t.unit);
            queue.wrong.extend(
                late.map(|u| format!("{u} was let go and queued again, and didn't finish in time")),
            );
            break;
        }
        thread::sleep(LOOK);
        queue.look(false);
    }
    match queue.wrong.as_slice() {
        [] => Ok(status),
        wrong => {
            let text = wrong.join("; ");
            complain(&text);
            cfg.journal("err", &text);
            Err(io::Error::other(text))
        }
    }
}

/// A job queued again that has to be waited for: one that didn't wait
/// behind the run when it was read back.
struct Tracked {
    id: u32,
    unit: String,
    unit_path: String,
}

/// systemd's queue as an activation began.
struct Queue<'c> {
    cfg: &'c Config,
    /// Every job there was, and the starts that were running, each with
    /// its unit's invocation then (`same_run`).
    existing: HashSet<u32>,
    running: HashMap<u32, Vec<u8>>,
    /// Jobs queued again that are waited for here.
    tracked: Vec<Tracked>,
    /// What went wrong with what it let go: a start lost, or failed.
    wrong: Vec<String>,
    /// Units already said to be let go.
    said: HashSet<String>,
}

impl<'c> Queue<'c> {
    fn now(cfg: &'c Config) -> Queue<'c> {
        let jobs = Queue::jobs_of(cfg).unwrap_or_default();
        Queue {
            cfg,
            existing: jobs.iter().map(Job::id).collect(),
            running: jobs
                .iter()
                .filter(|j| j.running() && j.kind() == "start")
                .filter_map(|j| known(Queue::invocation(cfg, j)).map(|i| (j.id(), i)))
                .collect(),
            tracked: Vec::new(),
            wrong: Vec::new(),
            said: HashSet::new(),
        }
    }

    /// One look: what's held is let go and queued again (with `release`),
    /// and what's tracked is followed. A queue it can't read is left as it
    /// is.
    fn look(&mut self, release: bool) {
        if self.running.is_empty() && self.tracked.is_empty() {
            return;
        }
        let Some(jobs) = Queue::jobs_of(self.cfg) else {
            return;
        };
        if release && !self.running.is_empty() {
            let touched = self.touched();
            for (job, run) in self.held_in(&jobs, &touched) {
                self.release(&touched, &job, &run);
            }
        }
        self.follow();
    }

    /// The jobs among `jobs` that are runs from before the activation.
    fn runs(&self, jobs: &[Job]) -> HashSet<u32> {
        jobs.iter()
            .filter(|j| j.running())
            .filter(|j| {
                same_run(
                    self.running.get(&j.id()),
                    j.kind(),
                    Queue::invocation(self.cfg, j),
                )
            })
            .map(Job::id)
            .collect()
    }

    /// The jobs to let go among `jobs`, as `held` finds them.
    fn held_in(&self, jobs: &[Job], touched: &HashSet<String>) -> Vec<(Job, Job)> {
        let cfg = self.cfg;
        let runs = self.runs(jobs);
        held(jobs, touched, &self.existing, &runs, |j| {
            call(cfg, &[j.path(), JOB, "GetBefore"]).unwrap_or_default()
        })
        .into_iter()
        .map(|(j, r)| (j.clone(), r))
        .collect()
    }

    /// The run `job` waits behind among `jobs`, if all it waits for is runs
    /// from before the activation.
    fn behind(&self, jobs: &[Job], job: &Job) -> Option<Job> {
        let cfg = self.cfg;
        let runs = self.runs(jobs);
        Graph::new(&runs, |j: &Job| {
            call(cfg, &[j.path(), JOB, "GetBefore"]).unwrap_or_default()
        })
        .behind(job)
    }

    /// The invocation of a job's unit: a new run of it has another.
    fn invocation(cfg: &Config, job: &Job) -> Option<Vec<u8>> {
        property::<Vec<u8>>(cfg, job.unit_path(), UNIT, "InvocationID")
    }

    /// One held job, let go and queued again, unless something would be
    /// lost by it or it isn't held any more. Everything is read again just
    /// before Cancel: new blockers, new consumers, a job whose kind changed
    /// and a run that ended or was replaced show in that read and keep it.
    /// The unit is put where the next activation starts it again before the
    /// job is cancelled, and taken out once it is queued again. What's left
    /// is the gap between the last read and Cancel (D-Bus has no
    /// cancel-if-waiting): the job queued again is read back, and if it
    /// doesn't wait behind the run it is waited for here (`follow`).
    fn release(&mut self, touched: &HashSet<String>, job: &Job, run: &Job) {
        let cfg = self.cfg;
        let Some(jobs) = Queue::jobs_of(cfg) else {
            return;
        };
        let Some((job, run)) = self
            .held_in(&jobs, touched)
            .into_iter()
            .find(|(j, r)| j.id() == job.id() && j.kind() == job.kind() && r.id() == run.id())
        else {
            return;
        };
        let Some(method) = queued_by(job.kind()) else {
            return;
        };
        // What requires it, or must follow it, would fail or run early.
        if self.bound(&jobs, &job)
            || !only_targets_follow(&job, |j| call(cfg, &[j.path(), JOB, "GetAfter"]))
        {
            return;
        }
        let read = |j: &Job, p: &str| property::<String>(cfg, j.path(), JOB, p);
        if read(&job, "State").as_deref() != Some("waiting")
            || read(&job, "JobType").as_deref() != Some(job.kind())
            || read(&run, "State").as_deref() != Some("running")
        {
            return;
        }
        let recovery = cfg.activation_lists.join(RECOVERY);
        let added = match record(&recovery, job.unit()) {
            Ok(added) => added,
            Err(e) => {
                let line = format!(
                    "{} kept waiting: it could not be kept for recovery in {}: {e}",
                    job.unit(),
                    recovery.display()
                );
                complain(&line);
                cfg.journal("err", &line);
                return;
            }
        };
        // A job that ended meanwhile is left be.
        if call::<()>(cfg, &[job.path(), JOB, "Cancel"]).is_none() {
            if added {
                let _ = unrecord(&recovery, job.unit());
            }
            return;
        }
        let again = (0..3).find_map(|_| {
            call::<String>(cfg, &[SYSTEMD[1], MANAGER, method, "ss", job.unit(), MODE])
                .as_deref()
                .and_then(job_id)
        });
        let Some(again) = again else {
            // It stays in the recovery list.
            let line = format!(
                "{} {} was let go and could not be queued again",
                job.unit(),
                job.kind()
            );
            complain(&line);
            cfg.journal("err", &line);
            self.wrong.push(format!(
                "{} was let go and could not be queued again: its start would be lost",
                job.unit()
            ));
            return;
        };
        // Read back: still behind the run, or waited for here.
        let behind = Queue::jobs_of(cfg).and_then(|jobs| {
            let now = jobs.iter().find(|j| j.id() == again)?.clone();
            self.behind(&jobs, &now)
        });
        if behind.is_none() {
            self.tracked.push(Tracked {
                id: again,
                unit: job.unit().to_string(),
                unit_path: job.unit_path().to_string(),
            });
        }
        if added && let Err(e) = unrecord(&recovery, job.unit()) {
            let line = format!(
                "{} could not be taken out of {}: {e}",
                job.unit(),
                recovery.display()
            );
            complain(&line);
            cfg.journal("warning", &line);
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

    /// The jobs queued again that didn't wait behind the run, followed: one
    /// that waits behind a run from before again is let be, as any let go;
    /// one that has ended is done, unless its unit failed; one whose unit is
    /// still starting is waited for.
    fn follow(&mut self) {
        if self.tracked.is_empty() {
            return;
        }
        let Some(jobs) = Queue::jobs_of(self.cfg) else {
            return;
        };
        let tracked = std::mem::take(&mut self.tracked);
        for t in tracked {
            if let Some(job) = jobs.iter().find(|j| j.id() == t.id) {
                if self.behind(&jobs, job).is_none() {
                    self.tracked.push(t);
                }
                continue;
            }
            match property::<String>(self.cfg, &t.unit_path, UNIT, "ActiveState").as_deref() {
                Some("failed") => self.wrong.push(format!(
                    "{} was let go and queued again, and failed",
                    t.unit
                )),
                Some("activating" | "deactivating" | "reloading" | "refreshing") | None => {
                    self.tracked.push(t)
                }
                Some(_) => {}
            }
        }
    }

    /// Whether systemd keeps a link between the job and another one that
    /// letting it go would break: a unit that requires its unit, is bound to
    /// it or has it as requisite has a job queued, and would fail with this
    /// one's cancelling; or a unit its unit requires, is bound to or has as
    /// requisite has a job queued, which this one fails with, and which it
    /// wouldn't be linked to once queued again without its requirements. A
    /// list that can't be read counts as one.
    fn bound(&self, jobs: &[Job], job: &Job) -> bool {
        let queued: HashSet<&str> = jobs.iter().map(Job::unit).collect();
        let unit = job.unit_path();
        [
            "RequiredBy",
            "BoundBy",
            "RequisiteOf",
            "Requires",
            "BindsTo",
            "Requisite",
        ]
        .iter()
        .any(|p| {
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

/// `unit` put in the recovery list at `path`, unless it is there already:
/// whether it was put there.
pub fn record(path: &Path, unit: &str) -> io::Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if text.lines().any(|l| l == unit) {
        return Ok(false);
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{unit}")?;
    f.sync_all()?;
    Ok(true)
}

/// `unit` taken back out of the recovery list at `path`, the rest left.
pub fn unrecord(path: &Path, unit: &str) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(i) = lines.iter().rposition(|l| *l == unit) {
        lines.remove(i);
    }
    let rest: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let tmp: PathBuf = path.with_extension("burrow-switch");
    fs::write(&tmp, rest)?;
    fs::rename(&tmp, path)
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
mod tests;
