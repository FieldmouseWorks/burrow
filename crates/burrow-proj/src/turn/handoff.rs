//! The records a handed-off turn (`proj handoff`) passes to the turn that takes
//! its place: its marker, its captured GitHub awaits, and the records that name
//! it (the quota registration, the question recommendation). Every write is
//! checked and undone together: a turn that can't take all of them over is not
//! started, and a turn that isn't started gives them all back.

use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::job::Job;
use crate::{read, systemd};

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
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
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
        if let Err(e) = crate::swap(path, new) {
            let mut said = format!("{}: {e}", path.display());
            for (path, old, _) in &changes[..i] {
                if let Err(e) = crate::swap(path, old) {
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
        match fs::read(archive(job, from)) {
            Ok(bytes) => crate::state_reads::write(archive(job, to), bytes)
                .map_err(|e| format!("{}: {e}", archive(job, to).display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", archive(job, from).display())),
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

/// Whether turn `n`, a handoff's replacement, was accepted: launched, queued
/// for a slot, or past that. Having a number says nothing; it is written before
/// the turn's records are.
pub(crate) fn accepted(cfg: &Config, job: &Job, n: &str) -> bool {
    let parked = fs::read(crate::policy::path(job, "park"))
        .ok()
        .and_then(|b| serde_json::from_slice::<crate::policy::Park>(&b).ok())
        .is_some_and(|p| p.turn == n);
    parked
        || job.turn(n, "slot").exists()
        || read(&job.turn(n, "outcome")).is_some_and(|o| !o.trim().is_empty())
        || systemd::show(cfg, &job.unit(), "LoadState") == "loaded"
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
