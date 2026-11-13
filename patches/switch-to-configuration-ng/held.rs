//! burrow (burrow#811, modules/switch.nix): an activation never waits on a
//! job queued behind a run that was already going when it began.
//!
//! switch-to-configuration waits on every job it queues, and systemd holds a
//! job back while a job it is ordered after hasn't ended. So a unit it
//! restarts that is ordered after a long oneshot already running (a sweep
//! after a two-hour renovate) holds the activation until that run ends, past
//! any deadline a caller sets.
//!
//! So `block_on_jobs` stops waiting once all it waits on is jobs that wait,
//! directly or through other waiting jobs, on nothing but starts that were
//! running as the activation began. Nothing is cancelled or queued again:
//! the jobs stay in systemd's queue as they are, with their ordering, their
//! requirements and whatever merged into them, and run once the run ends.
//! Their results still count if they end before the activation does. A job
//! that also waits on anything else (a new unit that is starting) is still
//! waited on whole, so a start that hangs still holds the activation. systemd
//! says what each job waits on (org.freedesktop.systemd1.Job.GetBefore), so
//! nothing here models its ordering; anything that can't be read keeps the
//! wait, as it was.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use dbus::blocking::{stdintf::org_freedesktop_dbus::Properties, LocalConnection, Proxy};

const SYSTEMD: &str = "org.freedesktop.systemd1";
const ROOT: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const JOB: &str = "org.freedesktop.systemd1.Job";
const UNIT: &str = "org.freedesktop.systemd1.Unit";
const TIMEOUT: Duration = Duration::from_secs(25);

/// A job as systemd lists one (ListJobs, GetBefore).
type Listed = (
    u32,
    String,
    String,
    String,
    dbus::Path<'static>,
    dbus::Path<'static>,
);

/// A job: its id, unit, type, state, object and its unit's object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u32,
    pub unit: String,
    pub kind: String,
    pub state: String,
    pub path: String,
    pub unit_path: String,
}

impl From<Listed> for Job {
    fn from((id, unit, kind, state, path, unit_path): Listed) -> Job {
        Job {
            id,
            unit,
            kind,
            state,
            path: path.to_string(),
            unit_path: unit_path.to_string(),
        }
    }
}

impl Job {
    fn running(&self) -> bool {
        self.state == "running"
    }
    fn waiting(&self) -> bool {
        self.state == "waiting"
    }
}

/// The starts that were running as the activation began, each with its
/// unit's invocation then: none until `begin`.
static BEFORE: OnceLock<HashMap<u32, Vec<u8>>> = OnceLock::new();

thread_local! {
    /// The jobs already said to be let be.
    static SAID: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
}

/// Noted as the activation begins: the starts running now.
pub fn begin(conn: &LocalConnection) {
    let runs = list(conn)
        .unwrap_or_default()
        .into_iter()
        .filter(|j| j.running() && j.kind == "start")
        .filter_map(|j| known(invocation(conn, &j)).map(|i| (j.id, i)))
        .collect();
    let _ = BEFORE.set(runs);
}

/// Whether all of `waited`, the jobs the activation still waits on, are to
/// be let be: each said, once.
pub fn only_held<'p>(
    conn: &LocalConnection,
    waited: impl IntoIterator<Item = &'p dbus::Path<'static>>,
) -> bool {
    let Some(before) = BEFORE.get().filter(|b| !b.is_empty()) else {
        return false;
    };
    let Some(jobs) = list(conn) else {
        return false;
    };
    let runs: HashSet<u32> = jobs
        .iter()
        .filter(|j| j.running() && before.contains_key(&j.id))
        .filter(|j| same_run(before.get(&j.id), &j.kind, invocation(conn, j)))
        .map(|j| j.id)
        .collect();
    if runs.is_empty() {
        return false;
    }
    let waited: Vec<Option<Job>> = waited
        .into_iter()
        .map(|p| jobs.iter().find(|j| j.path == **p).cloned())
        .collect();
    let Some(held) = held(&waited, &runs, |j| waits_for(conn, j)) else {
        return false;
    };
    SAID.with(|said| {
        let mut said = said.borrow_mut();
        for (job, run) in held {
            if said.insert(job.id) {
                let line = format!(
                    "{} {} waits behind {} (job {}, running since before the activation): not waited for, it runs once that ends",
                    job.unit, job.kind, run.unit, run.id
                );
                eprintln!("{line}");
                log::warn!("{line}");
            }
        }
    });
    true
}

/// A unit's invocation, as systemd says it, if it names one: an empty or
/// all-zero id is none, and none is never the same run as another.
pub fn known(invocation: Option<Vec<u8>>) -> Option<Vec<u8>> {
    invocation.filter(|i| i.iter().any(|b| *b != 0))
}

/// Whether a job running now is the run `then` was, as the activation
/// began: a start then and now, of the same known invocation. A start
/// merged with anything but a start changes its kind, and one merged with a
/// start is the same startup.
pub fn same_run(then: Option<&Vec<u8>>, kind: &str, now: Option<Vec<u8>>) -> bool {
    kind == "start" && then.is_some() && then == known(now).as_ref()
}

/// If every job of `waited` (each as systemd lists it now, or `None` once
/// it has ended) waits, directly or through other waiting jobs, on nothing
/// but jobs of `runs`, each with a run it waits on. A job that has ended is
/// waited for (its result is on its way), and so is a stop. `waits_for`
/// says what a job waits for, as systemd does.
pub fn held(
    waited: &[Option<Job>],
    runs: &HashSet<u32>,
    waits_for: impl FnMut(&Job) -> Vec<Job>,
) -> Option<Vec<(Job, Job)>> {
    let mut graph = Graph::new(runs, waits_for);
    let mut held = Vec::new();
    for job in waited {
        let job = job.as_ref()?;
        if job.kind == "stop" {
            return None;
        }
        let run = graph.behind(job)?;
        held.push((job.clone(), run));
    }
    Some(held).filter(|h| !h.is_empty())
}

/// What waits on what, as systemd says it, each job asked once.
pub struct Graph<'r, F> {
    runs: &'r HashSet<u32>,
    waits_for: F,
    before: HashMap<u32, Vec<Job>>,
    /// For each job, the run it ends up waiting on if all it waits for is
    /// waiting jobs that do, or such a run: `None` if anything else.
    memo: HashMap<u32, Option<Job>>,
}

impl<'r, F: FnMut(&Job) -> Vec<Job>> Graph<'r, F> {
    pub fn new(runs: &'r HashSet<u32>, waits_for: F) -> Self {
        Graph {
            runs,
            waits_for,
            before: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    fn ahead(&mut self, job: &Job) -> Vec<Job> {
        if let Some(known) = self.before.get(&job.id) {
            return known.clone();
        }
        let ahead = (self.waits_for)(job);
        self.before.insert(job.id, ahead.clone());
        ahead
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
        if let Some(known) = self.memo.get(&job.id) {
            return known.clone();
        }
        // A wait that goes round holds on to nothing new.
        if !path.insert(job.id) {
            return None;
        }
        let ahead = self.ahead(job);
        let mut run = None;
        let mut pure = !ahead.is_empty();
        for other in &ahead {
            if other.running() && self.runs.contains(&other.id) {
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
        path.remove(&job.id);
        let result = run.filter(|_| pure);
        self.memo.insert(job.id, result.clone());
        result
    }
}

fn list(conn: &LocalConnection) -> Option<Vec<Job>> {
    let (jobs,): (Vec<Listed>,) = Proxy::new(SYSTEMD, ROOT, TIMEOUT, conn)
        .method_call(MANAGER, "ListJobs", ())
        .ok()?;
    Some(jobs.into_iter().map(Job::from).collect())
}

/// What a job waits for: nothing, when that can't be read, which keeps
/// the wait.
fn waits_for(conn: &LocalConnection, job: &Job) -> Vec<Job> {
    let Ok(path) = dbus::Path::new(job.path.as_str()) else {
        return Vec::new();
    };
    Proxy::new(SYSTEMD, path, TIMEOUT, conn)
        .method_call::<(Vec<Listed>,), _, _, _>(JOB, "GetBefore", ())
        .map(|(jobs,)| jobs.into_iter().map(Job::from).collect())
        .unwrap_or_default()
}

/// The invocation of a job's unit: a new run of it has another.
fn invocation(conn: &LocalConnection, job: &Job) -> Option<Vec<u8>> {
    let path = dbus::Path::new(job.unit_path.as_str()).ok()?;
    Proxy::new(SYSTEMD, path, TIMEOUT, conn)
        .get::<Vec<u8>>(UNIT, "InvocationID")
        .ok()
}

#[cfg(test)]
mod tests;
