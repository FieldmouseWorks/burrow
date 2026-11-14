//! The machine-only part of a sorting job's end. When the job ends (proj
//! _ended, [`ended`]), and first thing in every pass before it asks GitHub
//! anything ([`Pass::settle`]), the job is settled: its answer for its last
//! turn, or that it gave none, is kept with the job's own host-owned control
//! files ([`crate::ask::host_path`]), naming that turn. A record whose job is
//! settled holds no sorting slot ([`Pass::holds_sorting_slot`]), so the slot is
//! free as soon as the job ends, even while its own end unit is still running
//! and while GitHub refuses every call. A record whose job is gone is retired.
//! What the verdict does on GitHub (its `plan`, counted off in `did`) is left
//! to the pass ([`Pass::verdicts`]), which reads the answer from here and runs
//! when GitHub answers.
//!
//! The settlement is the job's, not its record's: an end hook that runs late
//! writes only its own job's answer, and a record that now names another job
//! (the issue sorted again) never reads it. It's taken under the job's lock,
//! shared, so no turn is being prepared, queued or restored meanwhile, and
//! only for a turn that has its own end on record. That lock is never waited
//! for: a launcher or a stop holding it (a reply waiting out this very end
//! unit, a retry's cancellation finishing the turn) means the next pass
//! settles it instead.

use std::fs;

use serde::{Deserialize, Serialize};

use super::{Pass, dirs};
use crate::config::Config;
use crate::job::Job;
use crate::{filled, read, swap};

/// What a sorting job's last turn said, as its end kept it.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Settled {
    /// The turn it was read from.
    pub turn: String,
    /// Its answer; None when it gave none (its engine refused it, say).
    pub answer: Option<String>,
}

/// Where a job's settlement is kept: with its host-owned control files, which
/// the job itself can't write.
fn kept(job: &Job) -> std::path::PathBuf {
    crate::ask::host_path(job, "sorting-ended")
}

/// A sorting job `id` has just ended: settled, with no GitHub call. Run by its
/// end unit ([`crate::cmd::finish`]) after the end's own holds are written and
/// before proj's queue is woken, so a sorting job waiting for this slot is let
/// in at once. Only this job is let off waiting for its end unit to stop.
pub(crate) fn ended(cfg: &Config, id: &str) {
    let Some(inbox) = &cfg.inbox else { return };
    if inbox.triage.is_none() {
        return;
    }
    let sorting = dirs(&inbox.state.join(".triage"), "")
        .iter()
        .any(|d| read(&d.join("job")).as_deref() == Some(id));
    if sorting {
        settle(cfg, &Job::new(cfg, id), true);
    }
}

/// The job settled when its last turn has ended for good with nothing still
/// outstanding: its end on record (its outcome, or `_ended`'s mark), no
/// question, retry or await, its units known to have stopped (its end unit's
/// too, unless this is that unit), and not held by policy: neither parked nor
/// a restoration its `_exec` hasn't accepted, which policy relaunches, or rolls
/// back and reuses the number of, if it fails or dies first
/// ([`crate::policy::pending`]). Never waits for the job's lock.
fn settle(cfg: &Config, job: &Job, in_end: bool) {
    let Ok(Some(_held)) = job.try_read_lock() else {
        return;
    };
    let Some(n) = job.turns() else { return };
    if settled(job).is_some() || crate::policy::pending(job) || crate::policy::denied(job) {
        return;
    }
    if !filled(&job.turn(&n, "outcome")) && !job.turn(&n, "ended").exists() {
        return;
    }
    if matches!(job.state().as_str(), "queued" | "running")
        || !crate::systemd::terminated(cfg, &job.unit())
        || (!in_end && !crate::systemd::terminated(cfg, &job.end_unit()))
    {
        return;
    }
    if !job.holds().is_ok_and(|h| h.is_empty()) {
        return;
    }
    // Read under the shared lock this holds, which `told`'s own shared
    // lock doesn't wait for.
    let record = Settled {
        answer: crate::cmd::told(cfg, job.id, &n).ok(),
        turn: n,
    };
    if let Ok(text) = serde_json::to_string(&record) {
        let _ = swap(&kept(job), &text);
    }
}

/// The job's settled answer, when it was settled from its last turn and
/// policy can't run that turn again: a settlement never outlives a relaunch.
pub(super) fn settled(job: &Job) -> Option<Settled> {
    let kept: Settled = serde_json::from_slice(&fs::read(kept(job)).ok()?).ok()?;
    (job.turns().as_deref() == Some(kept.turn.as_str()) && !crate::policy::pending(job))
        .then_some(kept)
}

impl Pass<'_> {
    /// Every sorting record's job settled that can be, before the pass asks
    /// GitHub anything: an end whose own settling was missed (its unit
    /// killed, its job's lock held then) is caught up here. A record whose
    /// job is gone, with no failed sorting to keep, is retired.
    pub(super) fn settle(&self) {
        if self.inbox.triage.is_none() {
            return;
        }
        for d in dirs(&self.sorting_dir(), "") {
            let Some(id) = read(&d.join("job")).filter(|j| !j.is_empty()) else {
                continue;
            };
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() {
                if !d.join("failed").exists() {
                    let _ = fs::remove_dir_all(&d);
                }
                continue;
            }
            settle(self.cfg, &job, false);
        }
    }

    /// Whether the record's job is settled.
    pub(super) fn record_settled(&self, d: &std::path::Path) -> bool {
        read(&d.join("job"))
            .filter(|j| !j.is_empty())
            .is_some_and(|id| settled(&Job::new(self.cfg, &id)).is_some())
    }
}
