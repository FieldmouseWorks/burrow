//! A job's record: where it stands, what it waits on and what ends that,
//! since when, when it's overdue, what happens next, and how it got there
//! (docs/proj-design.md, "One record per item"). [`derive`] is the one
//! reading of the markers in a job's folder, its unit and its holds that
//! says so, and every view is the record printed: `proj state`
//! ([`Job::state`]), `proj lifecycle` and what a turn's end keeps
//! ([`Job::lifecycle`]), the stall watchdog's facts ([`Job::facts`]), the
//! snapshot's job rows and the slot view (snapshot.rs), and `proj why`.
//! It only reads: nothing in it settles, recovers or writes. [`note`],
//! which proj's commands and a turn's start and end call, keeps the
//! record's history on the host's side of the job, where its sandbox can't
//! write; the history is printed, and never read to answer where a job
//! stands.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::job::{Await, Job, Request, USAGE_LIMIT, awaits};
use crate::waits::{Facts, job_wait};
use crate::{filled, mtime, now, read, systemd, usage};

mod files;
mod marks;
#[cfg(test)]
mod tests;

pub use files::{JobFile, TurnFile};
pub(crate) use files::{Marker, TurnMarker};
pub(crate) use marks::{
    Unread, await_checked, await_text, await_written, awaiting, checkpoint, deadline, end,
    end_checked, end_said_at, ended_down, has_await, has_outcome, has_slot, left_open, mark,
    mark_turn, outcome_checked, outcome_said, slot, slot_taken, stopped, transferring, unstuck,
};

/// Where a job stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// The policy keeps it from running.
    Parked,
    /// Its turn waits for a slot, or for a transfer to resume it.
    Queued,
    /// Its turn holds a slot.
    Running,
    /// Its latest turn ended, as it wrote it, or failed when it wrote
    /// nothing.
    Ended(String),
}

impl fmt::Display for Stage {
    /// The word `proj state` says.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Stage::Parked => "parked",
            Stage::Queued => "queued",
            Stage::Running => "running",
            Stage::Ended(how) => how,
        })
    }
}

/// What a job waits on, one variant per line `proj lifecycle` can print
/// beside its stage, and the waits it prints none for: a slot, a place in
/// line and a policy park. #972 closes the type.
#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    /// The policy parked it, waiting for this when its park says.
    Policy(Option<String>),
    /// A slot, for its queued turn.
    Slot,
    /// A running turn's command waits in line for this, since then.
    Line { since: String, what: String },
    /// A committed transfer, until it resumes or is cancelled.
    Retry,
    /// A person's reply to its question, or its deadline.
    Ask,
    /// The owner's reply to its one-way question.
    Owner,
    /// A change to this PR or issue (proj await).
    Await(String),
    /// GitHub's quota, resetting then.
    Quota(i64),
    /// Claude's usage, which the machine was out of till then.
    Released(i64),
    /// An engine's usage, resetting then.
    Usage { engine: &'static str, reset: i64 },
}

impl Wait {
    /// The line `proj lifecycle` prints for it, if it prints one.
    pub fn line(&self) -> Option<String> {
        match self {
            Wait::Policy(_) | Wait::Slot | Wait::Line { .. } => None,
            Wait::Retry => Some("retry".into()),
            Wait::Ask => Some("ask".into()),
            Wait::Owner => Some("owner".into()),
            Wait::Await(target) => Some(format!("await {target}")),
            Wait::Quota(reset) => Some(format!("quota GitHub {reset}")),
            Wait::Released(at) => Some(format!("released Claude {at}")),
            Wait::Usage { engine, reset } => Some(format!("usage {engine} {reset}")),
        }
    }

    /// Whether a turn holds it open (a transfer, a question, an await), as
    /// against a slot, a place in line, a park or a limit: proj stop clears
    /// all of these.
    pub fn held(&self) -> bool {
        matches!(self, Wait::Retry | Wait::Ask | Wait::Owner | Wait::Await(_))
    }

    /// What it is, in a word or two: its lifecycle line, or a name for one
    /// that prints none.
    pub fn label(&self) -> String {
        self.line().unwrap_or_else(|| match self {
            Wait::Policy(_) => "policy".into(),
            Wait::Slot => "slot".into(),
            Wait::Line { what, .. } => format!("line {what}"),
            _ => unreachable!("every other wait has a line"),
        })
    }

    /// What ends it, in words.
    pub fn ends(&self) -> String {
        match self {
            Wait::Policy(Some(what)) => {
                format!("the settings letting it run (it waits for {what})")
            }
            Wait::Policy(None) => "the settings letting it run".into(),
            Wait::Slot => "a slot coming free and its turn being picked for it".into(),
            Wait::Line { what, .. } => format!("its command's turn at {what}"),
            Wait::Retry => "the transfer resuming it, or being cancelled".into(),
            Wait::Ask => "a reply to its question, or the question's deadline".into(),
            Wait::Owner => "the owner's reply to its one-way question".into(),
            Wait::Await(target) if target == USAGE_LIMIT => {
                "its engine's usage reset (proj-watch resumes it)".into()
            }
            Wait::Await(target) => format!("a change to {target} (proj-watch resumes it)"),
            Wait::Quota(reset) => format!("GitHub's quota resetting at {}", usage::when(*reset)),
            Wait::Released(at) => {
                format!(
                    "nothing more: Claude's usage was back at {}",
                    usage::when(*at)
                )
            }
            Wait::Usage { engine, reset } => {
                format!("{engine}'s usage resetting at {}", usage::when(*reset))
            }
        }
    }
}

/// A change the history noted: the stage and waits before and after, as
/// [`Record::summary`] says them, when, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub at: i64,
    pub from: String,
    pub to: String,
    pub why: String,
}

/// A job's record, as [`derive`] reads it.
#[derive(Clone, Debug)]
pub struct Record {
    pub stage: Stage,
    /// What it waits on: its stage's wait (a park, a slot, a place in
    /// line), what its turn holds open, and the limits it waits out.
    pub wait: Vec<Wait>,
    /// Why what its turn holds open couldn't be read, if it couldn't (and
    /// `wait` leaves it out): only a record that isn't there holds nothing.
    pub unread: Option<String>,
    /// When it entered this stage or wait, in seconds; 0 when nothing says.
    pub since: i64,
    /// When to look again without an event: the soonest reset it waits for.
    pub wake_at: Option<i64>,
    /// Past this, it's a stall: the watchdog's due.
    pub overdue_at: Option<i64>,
    /// What happens to it next, and what brings it about.
    pub next: String,
    /// The changes noted, oldest first. Printed, never read to say where
    /// it stands.
    pub history: Vec<Change>,
    /// Each PR or issue it awaits, as its await says.
    pub awaits: Vec<Await>,
    /// How its latest turn said it ends, a line each.
    pub ends: Vec<String>,
    /// What the watchdog reads of its latest turn; None before the first.
    pub facts: Option<Facts>,
    /// Its latest turn, as recorded; None before the first.
    pub turn: Option<Turn>,
}

/// A turn as proj recorded it: what it ran on and the session its engine
/// named. What a starter that reports a job's work (burrow-fix) says ran.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub n: String,
    /// Its N.engine, N.model and N.effort: no model or effort for the
    /// engine's own.
    pub engine: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The session its stream names, once the engine has named one.
    pub session: Option<String>,
}

/// Turn `n` of `job`, as recorded.
fn turn(job: &Job, n: &str) -> Turn {
    Turn {
        n: n.to_owned(),
        engine: read(&job.turn_file(n, TurnFile::Engine)),
        model: read(&job.turn_file(n, TurnFile::Model)),
        effort: read(&job.turn_file(n, TurnFile::Effort)),
        session: crate::engine::stream_session(&job.turn_file(n, TurnFile::Jsonl)),
    }
}

impl Record {
    /// What `proj lifecycle` prints of it: its stage, then a line for each
    /// wait that prints one. An error when what it waits on couldn't be
    /// read.
    pub fn lifecycle(&self) -> Result<(String, Vec<String>), String> {
        if let Some(e) = &self.unread {
            return Err(e.clone());
        }
        Ok((
            self.stage.to_string(),
            self.wait.iter().filter_map(Wait::line).collect(),
        ))
    }

    /// Its stage and waits in one line, as the history notes them.
    pub fn summary(&self) -> String {
        let mut labels: Vec<String> = self.wait.iter().map(Wait::label).collect();
        if self.unread.is_some() {
            labels.push("unreadable".into());
        }
        if labels.is_empty() {
            self.stage.to_string()
        } else {
            format!("{} ({})", self.stage, labels.join(", "))
        }
    }

    /// `proj why`: the record in words.
    pub fn why(&self, id: &str) -> String {
        let at = |t: i64| {
            if t > 0 {
                usage::when(t)
            } else {
                "an unknown time".into()
            }
        };
        let mut out = format!("{id} is {}, since {}.\n", self.stage, at(self.since));
        if self.wait.is_empty() && self.unread.is_none() {
            out.push_str("It waits on nothing.\n");
        }
        for wait in &self.wait {
            out.push_str(&format!(
                "It waits on {}, which ends with {}.\n",
                wait.label(),
                wait.ends()
            ));
        }
        if let Some(e) = &self.unread {
            out.push_str(&format!("What its turn holds open can't be read: {e}\n"));
        }
        if let Some(t) = self.wake_at {
            out.push_str(&format!("It's looked at again at {}.\n", at(t)));
        }
        if let Some(t) = self.overdue_at {
            out.push_str(&format!("It's overdue at {}.\n", at(t)));
        }
        out.push_str(&format!("Next: {}.\n", self.next));
        if !self.history.is_empty() {
            out.push_str("History:\n");
            for c in &self.history {
                out.push_str(&format!(
                    "  {}  {} -> {}  ({})\n",
                    at(c.at),
                    c.from,
                    c.to,
                    c.why
                ));
            }
        }
        out
    }

    /// `proj why --json`: the record whole.
    pub fn json(&self, id: &str) -> serde_json::Value {
        let wait =
            |w: &Wait| serde_json::json!({"wait": w.label(), "line": w.line(), "ends": w.ends()});
        serde_json::json!({
            "id": id,
            "stage": self.stage.to_string(),
            "wait": self.wait.iter().map(wait).collect::<Vec<_>>(),
            "unread": self.unread,
            "since": self.since,
            "wake_at": self.wake_at,
            "overdue_at": self.overdue_at,
            "next": self.next,
            "history": self.history.iter().map(|c| serde_json::json!({
                "at": c.at, "from": c.from, "to": c.to, "why": c.why,
            })).collect::<Vec<_>>(),
            "ends": self.ends,
            "turn": self.turn.as_ref().map(|t| serde_json::json!({
                "n": t.n, "engine": t.engine, "model": t.model, "effort": t.effort,
                "session": t.session,
            })),
        })
    }
}

/// The job's record, from what is on disk and in systemd now. It only
/// reads.
pub fn derive(job: &Job) -> Record {
    let stage = stage(job);
    let state = stage.to_string();
    let awaited = awaited(job);
    let facts = facts(job, &state, &awaited);
    let n = job.turns().unwrap_or_default();
    let mut wait = stage_waits(job, &stage, &n);
    let unread = match holds(job) {
        Ok(held) => {
            wait.extend(held);
            None
        }
        Err(e) => Some(e),
    };
    wait.extend(limits(job, &state, facts.as_ref()));
    let item = facts.as_ref().and_then(|f| job_wait(job.cfg, f));
    let since = match (&item, &facts) {
        (Some(i), _) => i.since as i64,
        (None, Some(f)) => match stage {
            Stage::Queued => f.queued,
            Stage::Running => f.slot,
            _ => f.ended,
        },
        (None, None) => 0,
    };
    let wake_at = wait
        .iter()
        .filter_map(|w| match w {
            Wait::Quota(reset) | Wait::Usage { reset, .. } => Some(*reset),
            _ => None,
        })
        .min();
    let next = match &item {
        Some(i) => i.phase.clone(),
        None => next(&stage, &wait, facts.as_ref()),
    };
    Record {
        stage,
        wait,
        unread,
        since,
        wake_at,
        overdue_at: item.map(|i| i.due as i64),
        next,
        history: history(job),
        awaits: awaited,
        ends: ends(job, &n),
        facts,
        turn: (!n.is_empty()).then(|| turn(job, &n)),
    }
}

/// Each PR or issue it awaits, as its await says.
pub(crate) fn awaited(job: &Job) -> Vec<Await> {
    match read(&job.marker(Marker::Await)) {
        Some(text) if filled(&job.marker(Marker::Await)) => awaits(&text),
        _ => Vec::new(),
    }
}

/// How turn N said it ends, a line each.
pub(crate) fn ends(job: &Job, n: &str) -> Vec<String> {
    crate::turn::lines(&job.turn_marker(n, TurnMarker::End))
}

/// What its stage waits on: the policy for a parked job, a slot for a
/// queued turn (one a transfer holds waits on that instead), and for a
/// running turn, each thing its commands wait in line for.
pub(crate) fn stage_waits(job: &Job, stage: &Stage, n: &str) -> Vec<Wait> {
    match stage {
        Stage::Parked => vec![Wait::Policy(crate::policy::waits_for(job))],
        Stage::Queued if !job.retry_pending(n) => vec![Wait::Slot],
        Stage::Running => crate::turn::lines(&job.turn_marker(n, TurnMarker::Wait))
            .into_iter()
            .filter_map(|l| {
                let mut fields = l.split('\t');
                let since = fields.next().unwrap_or_default().to_owned();
                let what = fields.next().unwrap_or(&l).to_owned();
                (!what.is_empty()).then_some(Wait::Line { since, what })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What happens next to a job the watchdog has no wait for.
fn next(stage: &Stage, wait: &[Wait], facts: Option<&Facts>) -> String {
    if let Some(w) = wait
        .iter()
        .find(|w| w.line().is_some() || matches!(w, Wait::Policy(_)))
    {
        return format!("it goes on once {}", w.ends());
    }
    match (stage, facts) {
        (Stage::Queued, _) => "its turn takes a slot once one is free and it's picked".into(),
        (Stage::Running, _) => "its turn runs to its end".into(),
        (_, Some(f)) if f.stopped => "nothing: proj stop ended it".into(),
        (_, Some(f)) if f.owned => "its starter acts on how its turn ended".into(),
        (Stage::Ended(how), _) if how == "done" => "nothing: it's done".into(),
        (_, None) => "nothing until its first turn is started".into(),
        _ => "nothing, until something resumes it".into(),
    }
}

/// A turn's file that jobs from before each turn kept its own have
/// unnumbered ([`Job::adopt`] moves it into place).
#[derive(Clone, Copy, PartialEq)]
enum Kept {
    Slot,
    Outcome,
    Result,
}

impl Kept {
    const ALL: [Kept; 3] = [Kept::Slot, Kept::Outcome, Kept::Result];

    /// Turn N's own.
    fn numbered(self, job: &Job, n: &str) -> PathBuf {
        match self {
            Kept::Slot => job.turn_marker(n, TurnMarker::Slot),
            Kept::Outcome => job.turn_marker(n, TurnMarker::Outcome),
            Kept::Result => job.turn_file(n, TurnFile::Result),
        }
    }

    /// The unnumbered one.
    fn unnumbered(self, job: &Job) -> PathBuf {
        job.unnumbered(match self {
            Kept::Slot => "slot",
            Kept::Outcome => "outcome",
            Kept::Result => "result",
        })
    }
}

/// Turn N's `kept` file, or, for a job from before each turn kept its
/// own, the unnumbered one where [`Job::adopt`] would move it into place:
/// when the job has a turn, the file is no older than that turn's prompt,
/// and, for an outcome, the unit that may still read it back is gone.
fn kept(job: &Job, n: &str, kept: Kept) -> PathBuf {
    let numbered = kept.numbered(job, n);
    if numbered.exists() || !crate::job::valid_turn(n) {
        return numbered;
    }
    let old = kept.unnumbered(job);
    let mtime = |p: &Path| fs::metadata(p).ok()?.modified().ok();
    match (mtime(&old), mtime(&job.prompt(n))) {
        (Some(t), prompt)
            if prompt.is_none_or(|p| p <= t)
                && (kept != Kept::Outcome || systemd::gone(job.cfg, &job.unit())) =>
        {
            old
        }
        _ => numbered,
    }
}

/// Turn N's answer: its N.result, or a job's from before each turn kept its
/// own, where [`Job::adopt`] would move it from.
pub(crate) fn result_file(job: &Job, n: &str) -> PathBuf {
    kept(job, n, Kept::Result)
}

/// Where it stands: parked while the policy keeps it; queued while a
/// transfer is pending, otherwise queued or running while its unit is up
/// (as its latest turn has a slot or not); then how that turn ended, as it
/// wrote it, or failed when it wrote nothing.
pub(crate) fn stage(job: &Job) -> Stage {
    if crate::policy::parked(job) {
        return Stage::Parked;
    }
    let n = job.turns().unwrap_or_default();
    if job.retry_pending(&n) {
        return Stage::Queued;
    }
    let outcome = kept(job, &n, Kept::Outcome);
    if systemd::active(job.cfg, &job.unit()) {
        if kept(job, &n, Kept::Slot).exists() {
            Stage::Running
        } else {
            Stage::Queued
        }
    } else if filled(&outcome) {
        Stage::Ended(read(&outcome).unwrap_or_default())
    } else {
        Stage::Ended("failed".into())
    }
}

/// What it holds open, whatever its latest turn's outcome (a failed resume
/// leaves its question, and its deadline resumes it): a transfer
/// ([`Wait::Retry`]), its question while it waits for a person (their
/// reply resumes it, and so does a reversible one's deadline, with its
/// default: ask::expire), the owner once a one-way one's deadline has
/// passed and it's left for the owner (only their reply resumes it), and
/// each PR or issue it awaits (proj-watch resumes it once one changes). A
/// job that holds anything is still at work. proj stop clears all of it.
/// An error when a record that would say can't be read: only one that
/// isn't there holds nothing.
pub(crate) fn holds(job: &Job) -> Result<Vec<Wait>, String> {
    let text = |path: &Path| match crate::state_reads::bytes(path) {
        Ok(b) => Ok(Some(
            String::from_utf8_lossy(&b)
                .trim_end_matches('\n')
                .to_owned(),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    };
    let there = |path: &Path| {
        path.try_exists()
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let mut held = Vec::new();
    if job.turns().is_some_and(|n| job.retry_pending(&n)) {
        held.push(Wait::Retry);
    }
    let asked = match text(&job.marker(Marker::Turns))?.filter(|n| crate::job::valid_turn(n)) {
        Some(n) => {
            !there(&job.turn_marker(&n, TurnMarker::Stopped))?
                && text(&job.turn_marker(&n, TurnMarker::End))?.as_deref() == Some("ask")
        }
        None => false,
    };
    // A question an accepted answer has retired is answered, though the
    // cleanup that removes its files ([`crate::question::recover`]) may not
    // have run yet: every reader sees it answered, recovered or not.
    let id = text(&crate::ask::host_path(job, "question-id"))?.unwrap_or_default();
    let retired = crate::question::retiring(job, &id);
    let question = !retired && text(&crate::ask::host_path(job, "question"))?.is_some();
    if !retired && text(&crate::ask::host_path(job, "question-needs-owner"))?.is_some() {
        held.push(Wait::Owner);
    } else if asked || question {
        held.push(Wait::Ask);
    }
    for a in awaits(&text(&job.marker(Marker::Await))?.unwrap_or_default()) {
        if !a.target.trim().is_empty() {
            held.push(Wait::Await(a.target.trim().to_owned()));
        }
    }
    Ok(held)
}

/// What a quota or usage limit holds a turn for.
fn limits(job: &Job, state: &str, facts: Option<&Facts>) -> Vec<Wait> {
    let mut held = Vec::new();
    if let Some(facts) = facts {
        if let Some((_, reset)) = facts.quota_reset {
            held.push(Wait::Quota(reset));
        }
        let reset = facts.usage_reset.map(|(_, reset)| reset).or_else(|| {
            (state == "queued")
                .then(|| facts.spent.as_ref().map(|s| s.release()))
                .flatten()
        });
        if matches!(state, "queued" | "running")
            && let Some(release) = usage::last_release(job.cfg)
            && job.engine_label(&facts.turn) == "Claude"
        {
            held.push(Wait::Released(release));
        }
        if let Some(reset) = reset {
            let engine = job.engine_label(&facts.turn);
            held.push(Wait::Usage { engine, reset });
        }
    }
    held
}

/// When turn N ended, in seconds; 0 while it's going. When it wrote its
/// outcome, or when [`Job::mark_ended`] saw it had. A look that finds neither
/// is one in the moment before its `_ended` (now, which that then fixes), or
/// at a turn being started, or one whose `_ended` never ran (the machine went
/// down): its last write is all that's known, and looking again doesn't move
/// it.
pub(crate) fn ended_at(job: &Job, n: &str, state: &str, pending: impl FnOnce() -> bool) -> i64 {
    if matches!(state, "queued" | "running") {
        return 0;
    }
    let first = |files: &[PathBuf]| files.iter().find_map(|p| mtime(p));
    first(&[
        kept(job, n, Kept::Outcome),
        job.turn_marker(n, TurnMarker::Ended),
    ])
    .or_else(|| pending().then(now))
    .or_else(|| first(&[job.turn_file(n, TurnFile::Jsonl), job.prompt(n)]))
    .unwrap_or(0)
}

/// What the stall watchdog's look reads of the latest turn; None before
/// the first.
fn facts(job: &Job, state: &str, awaited: &[Await]) -> Option<Facts> {
    let n = job.turns()?;
    let prompt = job.prompt(&n);
    let opt = |p: PathBuf| filled(&p).then(|| read(&p).unwrap_or_default());
    Some(Facts {
        id: job.id.to_owned(),
        owned: Request::peek(job).is_ok_and(|r| r.owned),
        unstick: job.marker(Marker::Unstick).exists(),
        stopped: job.turn_marker(&n, TurnMarker::Stopped).exists(),
        end: opt(job.turn_marker(&n, TurnMarker::End)),
        link: opt(job.file(JobFile::Link)),
        queued: mtime(&prompt).unwrap_or(0),
        slot: if job.retry_pending(&n) {
            0
        } else {
            mtime(&kept(job, &n, Kept::Slot)).unwrap_or(0)
        },
        ended: ended_at(job, &n, state, || job.end_pending()),
        // Its deadline as it stands, to the second (none without a
        // timeout): when it took its slot and its timeout, moved on by
        // any time it waited in line. And what it waits in line for now,
        // if anything.
        deadline: read(&job.turn_marker(&n, TurnMarker::Deadline))
            .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|t| t.parse::<i64>().ok())
            .map_or(0, |t| t / 1_000_000_000),
        wait: opt(job.turn_marker(&n, TurnMarker::Wait)).map(|w| {
            let line = w.lines().next().unwrap_or_default();
            let (since, what) = line.split_once('\t').unwrap_or((line, ""));
            (since.to_owned(), what.to_owned())
        }),
        awaits: awaited
            .iter()
            .map(|a| (a.target.clone(), a.since.clone()))
            .collect(),
        quota_reset: if state == "queued" {
            let engine = read(&job.turn_file(&n, TurnFile::Engine))
                .and_then(|s| crate::engine::Engine::parse(&s).ok())
                .unwrap_or(crate::engine::Engine::Claude);
            let since = mtime(&prompt).unwrap_or(0);
            crate::github_quota::admission_reset(job.cfg, job, engine)
                .filter(|reset| *reset > since)
                .map(|reset| (since, reset))
        } else if state == "running" {
            None
        } else {
            crate::github_quota::reset(job, &n)
        },
        usage_reset: awaited
            .iter()
            .find(|a| a.target == USAGE_LIMIT)
            .and_then(|a| Some((a.since.parse().ok()?, a.was.parse().ok()?))),
        spent: (read(&job.turn_file(&n, TurnFile::Engine)).as_deref() != Some("codex"))
            .then(|| usage::kept(job.cfg))
            .flatten(),
        turn: n,
        state: state.to_owned(),
    })
}

/// The history's file, on the host's side of the job.
fn history_path(job: &Job) -> PathBuf {
    crate::ask::host_path(job, "history")
}

/// The changes noted, oldest first. A line that isn't one is left out:
/// the history is a log, and says nothing of where the job stands.
fn history(job: &Job) -> Vec<Change> {
    read(&history_path(job))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            Some(Change {
                at: f.next()?.parse().ok()?,
                from: f.next()?.to_owned(),
                to: f.next()?.to_owned(),
                why: f.next()?.to_owned(),
            })
        })
        .collect()
}

/// Notes the job's record in its history when its stage or waits differ
/// from the last change noted, with why (the command or step that ran).
/// Called at the end of each proj command that changes a job, and in a
/// turn's start and end paths. A job with no folder has nothing to note.
pub fn note(job: &Job, why: &str) -> Result<(), String> {
    if !job.dir.is_dir() {
        return Ok(());
    }
    let path = history_path(job);
    let failed = |e: io::Error| format!("{}: {e}", path.display());
    crate::ask::ensure(job)?;
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .map_err(failed)?;
    // Two notes at once append one change, not two.
    f.lock().map_err(failed)?;
    // Derived under the lock, so a note that waited for another's can't
    // append a state from before it.
    // A job's own files say part of it (its outcome): one line, tabs out.
    let to = derive(job).summary().replace(['\t', '\n', '\r'], " ");
    let from = fs::read_to_string(&path)
        .map_err(failed)?
        .lines()
        .last()
        .and_then(|l| l.split('\t').nth(2))
        .unwrap_or("")
        .to_owned();
    if from == to {
        return Ok(());
    }
    let why = why.replace(['\t', '\n'], " ");
    writeln!(f, "{}\t{from}\t{to}\t{why}", now()).map_err(failed)
}

/// [`note`], told on stderr when it fails: the history is a log, and a
/// command that changed a job has done so whether it's noted or not.
pub fn noted(job: &Job, why: &str) {
    if let Err(e) = note(job, why) {
        eprintln!("proj: {}'s history: {e}", job.id);
    }
}
