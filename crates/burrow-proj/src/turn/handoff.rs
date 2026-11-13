//! The records a handed-off turn (`proj handoff`) passes to the turn that takes
//! its place: its marker, its captured GitHub awaits, and the records that name
//! it (the quota registration, the question recommendation). Every write is
//! checked and undone together: a turn that can't take all of them over is not
//! started, and a turn that isn't started gives them all back.

use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::job::Job;
use crate::systemd;

/// A turn that took over a handed-off one's waits: whose turn it took over.
pub(crate) const CARRY: &str = "carry";

/// The job's next turn takes a waiting one's place (`proj handoff`): what the
/// old turn registered carries over, its published and pending waits, their
/// baselines and quota registration, and a recommendation it left for its
/// question, none of it cleared as a fresh turn's start does. The old turn
/// keeps its own record.
pub(crate) fn start_handoff(cfg: &Config, id: &str, msg: &str) -> Result<(), String> {
    super::start_with(cfg, id, msg, false, crate::question::Answer::Handoff, None)
}

/// The most a record that names a turn may hold, and a captured-awaits archive.
const RECORD_LIMIT: u64 = 1 << 20;
const ARCHIVE_LIMIT: u64 = 16 << 20;

/// A job-state file read through the confined state APIs, which refuse a link
/// or a special file anywhere on its path, and no more than `limit` bytes of
/// it. None when it isn't there; too large, redirected or unreadable is an
/// error, never skipped.
fn read_bounded(path: &std::path::Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    match crate::state_reads::prefix(path, limit + 1) {
        Ok(bytes) if bytes.len() as u64 > limit => {
            Err(format!("{}: larger than {limit} bytes", path.display()))
        }
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The records that name a turn, as files: where each is, and what it holds.
fn records(job: &Job) -> [PathBuf; 2] {
    [job.path(crate::github_quota::PENDING), job.path("ask.next")]
}

/// Records naming turn `from` named `to` instead: all or none. A record that
/// can't be read or written is an error, never skipped, and one written before
/// the failure is written back as it was.
pub(super) fn retarget(job: &Job, from: &str, to: &str) -> Result<(), String> {
    let mut changes = Vec::new();
    for path in records(job) {
        let text = match read_bounded(&path, RECORD_LIMIT) {
            Ok(Some(bytes)) => {
                String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Ok(None) => continue,
            Err(e) => return Err(e),
        };
        let mut value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if value["turn"] != from {
            continue;
        }
        value["turn"] = to.into();
        changes.push((path, text, value.to_string()));
    }
    for (i, (path, _, new)) in changes.iter().enumerate() {
        if let Err(e) = crate::state_reads::write(path, new) {
            let mut said = format!("{}: {e}", path.display());
            for (path, old, _) in &changes[..i] {
                if let Err(e) = crate::state_reads::write(path, old) {
                    said.push_str(&format!("; and couldn't restore {}: {e}", path.display()));
                }
            }
            return Err(said);
        }
    }
    Ok(())
}

/// An error with what an undo couldn't put back.
pub(super) fn failed(undone: Option<String>, error: String) -> String {
    match undone {
        Some(why) => format!("{error}; and giving back its records failed: {why}"),
        None => error,
    }
}

/// Remove a record; one that isn't there is as wanted.
fn rm(path: &std::path::Path) -> Result<(), String> {
    match crate::state_reads::remove(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

fn archive(job: &Job, n: &str) -> PathBuf {
    job.turn(n, "github-awaits")
}

/// Turn `to` takes over turn `from`'s waits: its marker, a copy of its captured
/// awaits (so waits it registers, or repeats, join those and keep their
/// baselines), and the records that name it. Undone if any step fails.
pub(super) fn take_over(job: &Job, from: &str, to: &str) -> Result<(), String> {
    let marker = job.turn(to, CARRY);
    crate::state_reads::write(&marker, format!("{from}\n"))
        .map_err(|e| format!("{}: {e}", marker.display()))?;
    let result = (|| {
        if let Some(bytes) = read_bounded(&archive(job, from), ARCHIVE_LIMIT)? {
            crate::state_reads::write(archive(job, to), bytes)
                .map_err(|e| format!("{}: {e}", archive(job, to).display()))?;
        }
        retarget(job, from, to)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(e) => match give_back(job, from, to) {
            Ok(()) => Err(e),
            Err(again) => Err(format!("{e}; and giving them back failed: {again}")),
        },
    }
}

/// What [`take_over`] did, undone: `to`'s marker and copy removed, the records
/// back on `from`.
pub(super) fn give_back(job: &Job, from: &str, to: &str) -> Result<(), String> {
    retarget(job, to, from)?;
    for path in [job.turn(to, CARRY), archive(job, to)] {
        rm(&path)?;
    }
    Ok(())
}

/// What is known of turn `n`, a handoff's replacement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Fate {
    /// Launched, queued for a slot, or past that.
    Accepted,
    /// Known never to have been accepted: its preparation was abandoned, or
    /// its unit is confirmed absent with nothing queued.
    Unaccepted,
    /// The host can't tell (a failed query, a unit changing state): the
    /// successor stays owned by the handoff, neither adopted nor dropped.
    Unknown,
}

/// Whether turn `n` was accepted, from what the host wrote or systemd says.
/// Having a number says nothing, and neither does an outcome: recovery writes
/// `failed` for a successor that was never accepted.
pub(crate) fn fate(cfg: &Config, job: &Job, n: &str) -> Fate {
    let parked = fs::read(crate::policy::path(job, "park"))
        .ok()
        .and_then(|b| serde_json::from_slice::<crate::policy::Park>(&b).ok())
        .is_some_and(|p| p.turn == n);
    if parked || job.turn(n, "slot").exists() {
        return Fate::Accepted;
    }
    match crate::question::receipt_state(job, n) {
        Ok(Some((true, _))) => return Fate::Accepted,
        Ok(Some((false, true))) => return Fate::Unaccepted,
        Ok(_) => {}
        Err(_) => return Fate::Unknown,
    }
    let pending = systemd::pending(cfg, &job.unit());
    match (
        pending,
        systemd::show(cfg, &job.unit(), "LoadState").as_str(),
    ) {
        (Some(true), _) | (_, "loaded") => Fate::Accepted,
        (Some(false), "not-found") => Fate::Unaccepted,
        _ => Fate::Unknown,
    }
}

/// A replacement that was numbered but never accepted, given up: its prompt and
/// records gone, the job's turn count back at `from`, so asking again starts it
/// afresh.
pub(crate) fn abandon(job: &Job, from: &str, to: &str) -> Result<(), String> {
    give_back(job, from, to)?;
    for path in [job.path(&format!("prompt.{to}")), job.turn(to, "priority")] {
        rm(&path)?;
    }
    let n: u64 = from
        .parse()
        .map_err(|_| format!("turn {from} isn't a number"))?;
    job.set_turns(n)
        .map_err(|e| format!("{}: {e}", job.path("turns").display()))
}
