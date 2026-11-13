//! The machine-only part of a sorting job's end. When the job ends (proj
//! _ended, [`ended`]), and first thing in every pass before it asks GitHub
//! anything ([`Pass::settle`]), the record is settled: the job's answer for its
//! last turn, or that it gave none, is kept in the record as `ended`.
//! A settled record holds no sorting slot ([`Pass::holds_sorting_slot`]), so
//! the slot is free as soon as the job ends, even while its own end unit is
//! still running and while GitHub refuses every call. A record whose job is
//! gone is retired the same way. What the verdict does on GitHub (its `plan`,
//! counted off in `did`) is left to the pass ([`Pass::verdicts`]), which reads
//! the answer from here and runs when GitHub answers.
//!
//! Settling takes no lock: it writes the same thing whoever does it, and
//! names the turn it read, so an answer from before a later turn (a reply)
//! is never taken as that turn's.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Pass, dirs};
use crate::config::Config;
use crate::job::Job;
use crate::{read, swap};

/// The file in a sorting record that keeps its settled answer.
pub(super) const ENDED: &str = "ended";

/// What a sorting job's last turn said, as its record keeps it.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Settled {
    /// The turn it was read from.
    pub turn: String,
    /// Its answer; None when it gave none (its engine refused it, say).
    pub answer: Option<String>,
}

/// A sorting job `id` has just ended: its record settled, with no GitHub call.
/// Run by its end unit ([`crate::cmd::finish`]) after the end's own holds are
/// written and before proj's queue is woken, so a sorting job waiting for this
/// slot is let in at once.
pub(crate) fn ended(cfg: &Config, id: &str) {
    let Some(inbox) = &cfg.inbox else { return };
    if inbox.triage.is_none() {
        return;
    }
    for d in dirs(&inbox.state.join(".triage"), "") {
        if read(&d.join("job")).as_deref() == Some(id) {
            settle(cfg, &d, true);
        }
    }
}

/// The record settled when its job has ended for good with nothing still
/// outstanding: no question, retry or await, its units known to have
/// stopped (its end unit's too, unless this is that unit), and not held by
/// policy. A record whose job is gone, with no failed sorting to keep, is
/// retired. Whether it holds a settled answer now.
fn settle(cfg: &Config, d: &Path, in_end: bool) -> bool {
    let Some(id) = read(&d.join("job")).filter(|j| !j.is_empty()) else {
        return false;
    };
    let job = Job::new(cfg, &id);
    if !job.dir.is_dir() {
        if !d.join("failed").exists() {
            let _ = fs::remove_dir_all(d);
        }
        return false;
    }
    let Some(n) = job.turns() else { return false };
    if settled(d, &job).is_some() {
        return true;
    }
    if crate::policy::parked(&job) || crate::policy::denied(&job) {
        return false;
    }
    if matches!(job.state().as_str(), "queued" | "running")
        || !crate::systemd::terminated(cfg, &job.unit())
        || (!in_end && !crate::systemd::terminated(cfg, &job.end_unit()))
    {
        return false;
    }
    if !job.holds().is_ok_and(|h| h.is_empty()) {
        return false;
    }
    let record = Settled {
        answer: crate::cmd::told(cfg, &id, &n).ok(),
        turn: n,
    };
    let Ok(text) = serde_json::to_string(&record) else {
        return false;
    };
    swap(&d.join(ENDED), &text).is_ok()
}

/// The record's settled answer, when it was settled from its job's last turn.
pub(super) fn settled(d: &Path, job: &Job) -> Option<Settled> {
    let kept: Settled = serde_json::from_slice(&fs::read(d.join(ENDED)).ok()?).ok()?;
    (job.turns().as_deref() == Some(kept.turn.as_str())).then_some(kept)
}

impl Pass<'_> {
    /// Every sorting record settled that can be, before the pass asks GitHub
    /// anything: an end whose own settling was missed (its unit killed, a
    /// pass holding things up) is caught up here.
    pub(super) fn settle(&self) {
        if self.inbox.triage.is_none() {
            return;
        }
        for d in dirs(&self.sorting_dir(), "") {
            settle(self.cfg, &d, false);
        }
    }
}
