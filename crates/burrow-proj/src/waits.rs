//! What each job waits on, for the stall watchdog: [`job_wait`] says what one
//! job's is, [`rewatch`] tells the watchdog, and [`waiting`] reads what a
//! running turn waits in line for.
//!
//! Watched: a turn waiting in line, for a slot or inside its run (its clock
//! stands still meanwhile), for stallAfterMinutes, and while the machine is
//! out of usage, for that long after it resets; one running, until its
//! deadline and GRACE (proj stops it before then); one that ended without
//! saying what comes next even when asked again, at once, since nothing resumes
//! it; one that ended in a failure, for RESUME_GRACE from its end, since
//! proj-inbox resumes an issue's job seconds later when a reply waits; one
//! awaiting GitHub, for awaitHours and GRACE (proj-watch resumes it by then,
//! told nothing changed). Not watched:
//! a turn that asks a person (their reply resumes it) or is done, one proj
//! stop ended, and any ended turn of an --owned job.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use burrow_watchdog::watch::Item;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::job::{Job, USAGE_LIMIT};
use crate::slot::successor;
use crate::{GRACE, RESUME_GRACE, mtime, now, read, swap, usage};

/// What [`job_wait`] reads of a job's latest turn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub id: String,
    pub turn: String,
    /// What `proj state` says of it.
    pub state: String,
    /// Its starter acts on each turn's end itself (proj run --owned).
    pub owned: bool,
    /// It's the job that takes the stuck issue (proj run --unstick).
    pub unstick: bool,
    /// proj stop ended it.
    pub stopped: bool,
    /// How it said it ends: await, ask, done, or undeclared when it wouldn't.
    pub end: Option<String>,
    /// What the job is about.
    pub link: Option<String>,
    /// When it was started, took its slot and ended ([`Job::facts`]; 0 while
    /// it's queued or running), in seconds.
    pub queued: i64,
    pub slot: i64,
    pub ended: i64,
    /// When a running turn's timeout is up as it stands, in seconds; 0 for
    /// none.
    pub deadline: i64,
    /// What a running turn's command waits in line for, and since when, as
    /// its N.wait has them ([`waiting`]'s).
    pub wait: Option<(String, String)>,
    /// Each PR or issue it awaits, and since when, as its await has them.
    pub awaits: Vec<(String, String)>,
    /// The machine out of Claude usage, if it is (usage.rs).
    pub spent: Option<usage::Spent>,
    /// A Codex refusal waits for this reset, independently of Claude's hold.
    pub usage_reset: Option<(i64, i64)>,
    /// A GitHub API refusal waits independently of either engine.
    pub quota_reset: Option<(i64, i64)>,
}

/// One job's wait in the watchdog's form, or None when there's nothing to
/// watch.
pub fn job_wait(cfg: &Config, f: &Facts) -> Option<Item> {
    if f.state == "parked" {
        return None;
    }
    let stall = cfg.stall_queued();
    let turn = &f.turn;
    let mut kick = None;
    let (since, due, phase) = match f.state.as_str() {
        _ if !matches!(f.state.as_str(), "queued" | "running")
            && !f.stopped
            && let Some((since, reset)) = f.quota_reset =>
        {
            kick = Some(crate::watch::kick_for(crate::github_quota::LIMIT));
            (since, reset + GRACE, "waiting for GitHub quota reset; proj-watch registers saved waits and resumes work then".to_owned())
        }
        _ if !matches!(f.state.as_str(), "queued" | "running")
            && !f.stopped
            && let Some((since, reset)) = f.usage_reset =>
        {
            kick = Some(crate::watch::kick_for(USAGE_LIMIT));
            (
                since,
                reset + GRACE,
                "waiting for Codex's usage reset; proj-watch resumes it then".to_owned(),
            )
        }
        "queued" if let Some((since, reset)) = f.quota_reset => (
            since,
            (reset + stall)
                .max(f.queued + stall)
                .max(f.spent.as_ref().map_or(0, |s| s.release() + stall)),
            if reset > now() {
                format!(
                    "turn {turn} is waiting for GitHub quota reset at {}",
                    crate::usage::when(reset)
                )
            } else {
                format!(
                    "turn {turn} is waiting for a slot after GitHub quota reset at {}",
                    crate::usage::when(reset)
                )
            },
        ),
        // Out of usage, no turn takes a slot till it's back (usage.rs): each
        // has stallAfterMinutes from then, its reset or the sooner end the
        // record has, which stays a while after.
        "queued" if let Some(spent) = &f.spent => (
            f.queued,
            (f.queued + stall).max(spent.release() + stall),
            if spent.back.is_none() {
                format!("turn {turn} is waiting in line: {}", usage::line(spent))
            } else {
                format!(
                    "turn {turn} is waiting in line for a slot, which usage was out for till {}",
                    usage::when(spent.release())
                )
            },
        ),
        "queued" => (
            f.queued,
            f.queued + stall,
            if f.unstick {
                format!(
                    "turn {turn} is waiting for the --unstick slot, which another --unstick turn holds"
                )
            } else {
                format!("turn {turn} is waiting in line for a slot")
            },
        ),
        "running" => match &f.wait {
            Some((waited, what)) => {
                let since = seconds(waited)?;
                (
                    since,
                    since + stall,
                    format!("turn {turn} is running, and waiting in line: {what}"),
                )
            }
            None if f.deadline > 0 => (
                f.slot,
                f.deadline + GRACE,
                format!("turn {turn} is running"),
            ),
            None => return None,
        },
        state => {
            if f.stopped || f.owned {
                return None;
            }
            if state != "done" {
                (
                    f.ended,
                    f.ended + RESUME_GRACE,
                    format!("turn {turn} ended ({state}), and nothing resumed it"),
                )
            } else {
                match f.end.as_deref() {
                    Some("undeclared") => (
                        f.ended,
                        f.ended,
                        format!(
                            "turn {turn} ended without saying what comes next (proj await, ask or done), even when asked again, so nothing resumes it"
                        ),
                    ),
                    Some("await") => {
                        // The longest wait sets the deadline, and its repo's
                        // proj-watch is what's kicked first.
                        let awaits: Vec<(&str, i64)> = f
                            .awaits
                            .iter()
                            .filter_map(|(t, at)| Some((t.as_str(), seconds(at)?)))
                            .collect();
                        let (first, at) = awaits
                            .iter()
                            .copied()
                            .reduce(|a, b| if b.1 < a.1 { b } else { a })?;
                        let targets: Vec<&str> = awaits.iter().map(|(t, _)| *t).collect();
                        kick = Some(crate::watch::kick_for(first));
                        let hours = cfg.await_hours;
                        (
                            at,
                            at + hours as i64 * 3600 + GRACE,
                            format!(
                                "waiting on GitHub (proj await) for {}; proj-watch resumes it once one changes, or after {hours}h",
                                targets.join(", ")
                            ),
                        )
                    }
                    _ => return None,
                }
            }
        }
    };
    if since < 0 || due < 0 {
        return None;
    }
    Some(Item {
        repo: None,
        what: format!("proj job {}", f.id),
        phase,
        since: since as f64,
        due: due as f64,
        link: f.link.clone().filter(|l| !l.is_empty()),
        kick,
        // Waiting for a slot: another job would only join the line.
        line: f.state == "queued",
        // The stuck issue's own: its stall goes to the owner, since that
        // issue would only hand it to itself.
        unstick: f.unstick,
        // A turn in line is `line`'s: none waits on a job of its own.
        claude: false,
        refresh: false,
    })
}

/// Whole seconds, written as digits alone.
fn seconds(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// A running turn's wait in line, from its wait dir (each turn's
/// PROJ_WAIT_DIR): a command that waits in line for something shared, a
/// machine-wide lock, creates a file there, writes one line saying what it
/// waits for, and holds an exclusive flock(2) on the file while it waits,
/// removing it once it has its turn. The oldest one still held: when it was
/// written, and its line, at most 200 characters; None for none. A file
/// nobody holds is a waiter's that died mid-wait (or left it behind), so it
/// never stands for one: removed, once it's past the ten seconds a waiter
/// has to lock the file it just made. One that holds its file and never gets
/// its turn is the stall watchdog's ([`job_wait`]).
pub fn waiting(dir: &Path) -> Option<(i64, String)> {
    let now = now();
    let mut names: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    names.sort();
    let mut best: Option<(i64, &Path)> = None;
    for f in &names {
        if !f.is_file() {
            continue;
        }
        let Some(t) = mtime(f) else { continue };
        let held = match File::open(f).map(|file| file.try_lock()) {
            Ok(Ok(())) => false,
            Ok(Err(TryLockError::WouldBlock)) | Ok(Err(TryLockError::Error(_))) | Err(_) => true,
        };
        if !held {
            if now - t >= 10 {
                let _ = fs::remove_file(f);
            }
        } else if best.is_none_or(|(since, _)| t < since) {
            best = Some((t, f));
        }
    }
    let (since, f) = best?;
    let text = fs::read(f).unwrap_or_default();
    let line = text.split(|&b| b == b'\n').next().unwrap_or_default();
    let what: String = String::from_utf8_lossy(line)
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    Some((
        since,
        if what.is_empty() {
            "something".into()
        } else {
            what
        },
    ))
}

/// The waits of the jobs named, brought up to date for the stall watchdog:
/// jobs/.waits/ID has a job's ([`job_wait`]'s) while it has one, and the
/// watch file all of them, which the watchdog reads. Called after each
/// change to a job (a turn starting, taking its slot, ending, a stop), under
/// one lock, so the last look at a job is the one that stands, and the
/// watchdog is kicked when what it reads changed. A job whose last change
/// never got here (the machine went down mid-turn) keeps the wait it had,
/// whose deadline then catches it; one whose dir is gone has none. Without
/// the watchdog there's no watch file: nothing to do. Never the caller's
/// failure: a turn goes on whether or not the watchdog hears. By the deployed
/// proj's waits, when a turn outlives a switch ([`crate::slot`]).
pub fn rewatch(cfg: &Config, ids: &[&str]) {
    if let Some(d) = successor(cfg)
        && Command::new(d)
            .arg("_rewatch")
            .args(ids)
            .status()
            .is_ok_and(|s| s.success())
    {
        return;
    }
    if publish(cfg, ids).is_err() {
        eprintln!(
            "proj: couldn't update the stall watchdog's waits for {}",
            ids.join(" ")
        );
    }
}

/// [`rewatch`] for every job it watches, every one whose wait the policy
/// keeps from it for now ([`HELD`]), and every one still live ([`live`]):
/// the machine's usage changed ([`usage`]), which moves the deadline of every
/// turn in line, or the policy did, which lets a held one back, or the
/// watchdog was just enabled, and jobs already running never published to it.
/// Never a settled job, whose wait ended weeks ago.
pub fn rewatch_all(cfg: &Config) {
    let mut ids: Vec<String> = [".waits", HELD]
        .iter()
        .flat_map(|d| fs::read_dir(cfg.jobs.join(d)).into_iter().flatten())
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.ends_with(".new"))
        .collect();
    ids.extend(
        fs::read_dir(&cfg.jobs)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.') && live(cfg, n)),
    );
    ids.sort();
    ids.dedup();
    rewatch(cfg, &ids.iter().map(String::as_str).collect::<Vec<_>>());
}

/// A job with a turn queued or running, or outstanding awaits: work the
/// watchdog must see whether or not it ever heard of the job.
fn live(cfg: &Config, id: &str) -> bool {
    Job::new(cfg, id).facts().is_some_and(|f| {
        !f.stopped
            && !f.owned
            && (matches!(f.state.as_str(), "queued" | "running")
                || (f.state == "done" && f.end.as_deref() == Some("await")))
    })
}

/// [`rewatch`] in the background, never waited for: the watchdog's lock, or
/// a slow kick, holds up nothing.
pub fn rewatch_later(cfg: &Config, id: &str) {
    let (cfg, id) = (cfg.clone(), id.to_owned());
    std::thread::spawn(move || rewatch(&cfg, &[&id]));
}

/// The jobs (an empty file each) with a wait the policy keeps from the
/// watchdog while it denies or parks them: a pause, a scope that leaves
/// their repo out, an admission it can't read. They're watched again once
/// it lets them be ([`rewatch_all`]), however their turn ended; a job that
/// has no wait to keep (settled, stopped, done) has none here.
const HELD: &str = ".waits-held";

fn publish(cfg: &Config, ids: &[&str]) -> io::Result<()> {
    let Some(file) = &cfg.waits else {
        return Ok(());
    };
    let dir = cfg.jobs.join(".waits");
    fs::create_dir_all(&dir)?;
    let held = cfg.jobs.join(HELD);
    fs::create_dir_all(&held)?;
    let lock = OpenOptions::new()
        .append(true)
        .create(true)
        .open(cfg.jobs.join(".waits.lock"))?;
    lock.lock()?;
    for id in ids {
        let f = dir.join(id);
        let job = Job::new(cfg, id);
        let kept = crate::policy::parked(&job) || crate::policy::denied(&job);
        let wait = Some(&job)
            .filter(|job| job.dir.is_dir())
            .and_then(|job| job.facts())
            .and_then(|mut facts| {
                if let Some(released) =
                    read(&crate::policy::path(&job, "released")).and_then(|s| s.parse::<i64>().ok())
                {
                    facts.queued = facts.queued.max(released);
                }
                job_wait(cfg, &facts).map(|mut item| {
                    item.repo = crate::policy::repository(&job);
                    item
                })
            });

        // Held while the policy keeps it, watched once it doesn't. A parked
        // job's own facts say nothing ([`job_wait`]), so its park record
        // keeps it held too. The replacement is committed before the hold
        // is dropped, so a failed write leaves the job discoverable.
        let mark = held.join(id);
        let drop_file = |p: PathBuf| match fs::remove_file(p) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        if kept && (wait.is_some() || crate::policy::parked(&job)) {
            fs::write(&mark, "")?;
            drop_file(f.clone())?;
            drop_file(f.with_file_name(format!("{id}.new")))?;
        } else if let Some(item) = wait {
            swap(&f, &format!("{}\n", json!(item)))?;
            drop_file(mark)?;
        } else {
            drop_file(f.clone())?;
            drop_file(f.with_file_name(format!("{id}.new")))?;
            drop_file(mark)?;
        }
    }
    let mut names: Vec<String> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut items = Map::new();
    for name in names {
        if !cfg.jobs.join(&name).is_dir() {
            let _ = fs::remove_file(dir.join(&name));
            continue;
        }
        if let Some(v) = fs::read(dir.join(&name))
            .ok()
            .and_then(|t| serde_json::from_slice::<Value>(&t).ok())
        {
            items.insert(name, v);
        }
    }
    let items = Value::Object(items);
    let was = fs::read(file)
        .ok()
        .and_then(|t| serde_json::from_slice::<Value>(&t).ok())
        .and_then(|v| v.get("items").filter(|i| !i.is_null()).cloned())
        .unwrap_or_else(|| json!({}));
    let text = serde_json::to_string_pretty(&json!({"at": now(), "items": items}))?;
    swap(file, &format!("{text}\n"))?;
    if items != was && !cfg.kick.is_empty() {
        let url = format!("{}/watchdog", cfg.kick);
        if burrow_events::kick::kick(&url, Duration::from_secs(5)).is_err() {
            eprintln!("proj: couldn't kick watchdog");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
