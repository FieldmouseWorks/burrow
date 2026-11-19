//! A job's markers, both ways in. Whatever needs to know one asks the
//! record what it says ([`stopped`], [`outcome`], [`Job::turns`]...): one
//! reading each, here. A step that changes the job marks it through
//! [`Mark`], which writes and never reads, so no reader can use it to read
//! where a job stands its own way.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use super::Kept;
use super::files::{Marker, TurnMarker};
use crate::engine::last_result;
use crate::job::{Job, Request, valid_turn};
use crate::record::TurnFile;
use crate::{Fail, filled, read, systemd};

/// A marker, as a step that changes the job writes it: it puts, swaps or
/// removes the file and never reads it. Its path shows only in what a
/// failed write says.
pub(crate) struct Mark(PathBuf);

/// The job's marker `m`, to write.
pub(crate) fn mark(job: &Job, m: Marker) -> Mark {
    Mark(job.marker(m))
}

/// Turn N's marker `m`, to write.
pub(crate) fn mark_turn(job: &Job, n: &str, m: TurnMarker) -> Mark {
    Mark(job.turn_marker(n, m))
}

impl Mark {
    /// Written whole through the confined writer, for state the job's
    /// sandbox may write (`state_reads::write`).
    pub(crate) fn write(&self, text: impl AsRef<[u8]>) -> io::Result<()> {
        crate::state_reads::write(&self.0, text)
    }

    /// Swapped in through its `.new`, for state only the host writes
    /// ([`crate::swap`]).
    pub(crate) fn swap(&self, text: &str) -> io::Result<()> {
        crate::swap(&self.0, text)
    }

    /// Written in place, as a new job's files are (`fs::write`).
    pub(crate) fn put(&self, text: &str) -> io::Result<()> {
        fs::write(&self.0, text)
    }

    /// Removed through the confined remover (`state_reads::remove`).
    pub(crate) fn remove(&self) -> io::Result<()> {
        crate::state_reads::remove(&self.0)
    }

    /// Removed in place (`fs::remove_file`).
    pub(crate) fn remove_file(&self) -> io::Result<()> {
        fs::remove_file(&self.0)
    }

    /// A failed write, naming the file, as [`crate::io_fail`] says it.
    pub(crate) fn fail(&self, e: io::Error) -> Fail {
        crate::io_fail(&self.0, e)
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

/// A marker that's there but couldn't be read: one that isn't there is
/// none, never this. It names the file in what it says.
#[derive(Debug)]
pub(crate) struct Unread {
    path: PathBuf,
    error: io::Error,
}

impl Unread {
    /// As [`crate::io_fail`] says it.
    pub(crate) fn fail(self) -> Fail {
        crate::io_fail(&self.path, self.error)
    }

    /// The error alone.
    pub(crate) fn error(self) -> io::Error {
        self.error
    }
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.error)
    }
}

/// A marker's bytes through the confined reader; None when it isn't there.
fn checked(path: PathBuf) -> Result<Option<Vec<u8>>, Unread> {
    match crate::state_reads::bytes(&path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Unread { path, error }),
    }
}

impl Job<'_> {
    /// The latest turn's number, as written; None before the first.
    pub fn turns(&self) -> Option<String> {
        read(&self.marker(Marker::Turns)).filter(|n| valid_turn(n))
    }

    /// Swapped in whole, so a reader never finds it missing or half written.
    pub fn set_turns(&self, n: u64) -> io::Result<()> {
        crate::ask::ensure(self).map_err(io::Error::other)?;
        crate::state_reads::write(self.marker(Marker::Turns), format!("{n}\n"))
    }

    /// Jobs from before each turn kept its own files have the last turn's
    /// slot, outcome and answer unnumbered (a turn that ran across that
    /// update writes them there too, once it ends). They're turn N's if
    /// they're no older than its prompt; anything older is an earlier
    /// turn's, and goes.
    pub fn adopt(&self) {
        let Some(n) = self.turns() else { return };
        let prompt = mtime_of(&self.prompt(&n));
        for kept in Kept::ALL {
            let old = kept.unnumbered(self);
            let Some(t) = mtime_of(&old) else { continue };
            let newer = prompt.is_some_and(|p| p > t);
            // A turn still running the old code reads its outcome back once
            // it has written it, so that one stays until it has ended.
            if kept == Kept::Outcome && !newer && !systemd::gone(self.cfg, &self.unit()) {
                continue;
            }
            let numbered = kept.numbered(self, &n);
            if !numbered.exists() && !newer {
                let _ = fs::rename(&old, &numbered);
            } else {
                let _ = fs::remove_file(&old);
            }
        }
    }

    /// Whether a transfer's resumed turn N ran: it wrote an outcome, and
    /// didn't fail to start.
    pub(crate) fn retry_completed(&self, n: &str) -> bool {
        filled(&self.turn_marker(n, TurnMarker::Outcome))
            && !self.turn_marker(n, TurnMarker::RetryStartupFailed).exists()
    }

    /// Whether a transfer holds turn N, not stopped and not yet run.
    pub(crate) fn retry_pending(&self, n: &str) -> bool {
        transferring(self, n) && !stopped(self, n) && !self.retry_completed(n)
    }

    /// Whether turn N, the latest, was cut off: its whole unit killed (by oomd,
    /// as a rule), not stopped, with no answer or error but its session in its
    /// stream, and not a sandbox's, whose VM may outlive it. Under the job's
    /// lock.
    pub fn cut_off(&self, n: &str) -> bool {
        self.turns().as_deref() == Some(n)
            && self.state() == "failed"
            && !filled(&self.turn_marker(n, TurnMarker::Outcome))
            && !stopped(self, n)
            && Request::load(self).is_ok_and(|r| r.agent.is_none())
            && crate::engine::stream_names_session(&self.turn_file(n, TurnFile::Jsonl), 0)
            && last_result(&self.turn_file(n, TurnFile::Jsonl), 0).is_none()
    }

    /// How turn N ended, for its log: its outcome, or failed when it has
    /// none and isn't going (its unit stopped, or killed with it, before it
    /// wrote one), as `proj state` says. None while it's queued or running,
    /// a second run included, or for an earlier turn from before turns kept
    /// their outcomes (it has no slot either). Looked at under the lock
    /// turns start under, so no turn starts halfway through and has its
    /// state told as turn N's.
    pub fn ended(&self, n: &str) -> Option<String> {
        let _held = self.read_lock().ok()?;
        self.adopt();
        let outcome = self.turn_marker(n, TurnMarker::Outcome);
        if self.turns().as_deref() == Some(n) && self.retry_pending(n) {
            None
        } else if filled(&outcome) {
            read(&outcome)
        } else if self.turns().as_deref() == Some(n) {
            (!systemd::active(self.cfg, &self.unit())).then(|| "failed".into())
        } else if has_slot(self, n) {
            Some("failed".into())
        } else {
            None
        }
    }

    /// When a turn killed before it could write its outcome ended (what kills
    /// its whole cgroup takes everything in it along), from outside that
    /// cgroup: its `_ended`'s first act. Only `_ended` writes it, and a new turn
    /// waits out the last one's `_ended` before it's written, so this is always
    /// the turn that just ended, and no look at a turn being started can leave
    /// it on a live one. Kept once written, so nothing moves it.
    pub fn mark_ended(&self) {
        let Some(n) = self.turns() else { return };
        if !ended_down(self, &n) {
            let _ = mark_turn(self, &n, TurnMarker::Ended).write("");
        }
    }
}

/// When a file last changed, to the nanosecond, if it's there.
fn mtime_of(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// Whether proj stop stopped turn N.
pub(crate) fn stopped(job: &Job, n: &str) -> bool {
    job.turn_marker(n, TurnMarker::Stopped).exists()
}

/// Whether turn N's outcome is there, said or not yet.
pub(crate) fn has_outcome(job: &Job, n: &str) -> bool {
    job.turn_marker(n, TurnMarker::Outcome).exists()
}

/// Whether turn N's outcome says anything.
pub(crate) fn outcome_said(job: &Job, n: &str) -> bool {
    filled(&job.turn_marker(n, TurnMarker::Outcome))
}

/// Turn N's outcome's bytes, strictly: an error when it's there and can't
/// be read.
pub(crate) fn outcome_checked(job: &Job, n: &str) -> Result<Option<Vec<u8>>, Unread> {
    checked(job.turn_marker(n, TurnMarker::Outcome))
}

/// Whether turn N's end is written down: its outcome, or the mark its
/// `_ended` left ([`Job::mark_ended`]).
pub(crate) fn ended_down(job: &Job, n: &str) -> bool {
    has_outcome(job, n) || job.turn_marker(n, TurnMarker::Ended).exists()
}

/// How turn N said it ends: done, ask, await, undeclared.
pub(crate) fn end(job: &Job, n: &str) -> Option<String> {
    read(&job.turn_marker(n, TurnMarker::End))
}

/// [`end`]'s bytes, strictly: an error when it's there and can't be read.
pub(crate) fn end_checked(job: &Job, n: &str) -> Result<Option<Vec<u8>>, Unread> {
    checked(job.turn_marker(n, TurnMarker::End))
}

/// When turn N said how it ends.
pub(crate) fn end_said_at(job: &Job, n: &str) -> io::Result<SystemTime> {
    fs::metadata(job.turn_marker(n, TurnMarker::End))?.modified()
}

/// The slot turn N said it took.
pub(crate) fn slot(job: &Job, n: &str) -> Option<String> {
    read(&job.turn_marker(n, TurnMarker::Slot))
}

/// Whether turn N said it took a slot.
pub(crate) fn has_slot(job: &Job, n: &str) -> bool {
    job.turn_marker(n, TurnMarker::Slot).exists()
}

/// When turn N said it took its slot, in nanoseconds.
pub(crate) fn slot_taken(job: &Job, n: &str) -> Option<i64> {
    crate::mtime_nanos(&job.turn_marker(n, TurnMarker::Slot))
}

/// Turn N's deadline, in nanoseconds, as written; None for none.
pub(crate) fn deadline(job: &Job, n: &str) -> Option<i64> {
    read(&job.turn_marker(n, TurnMarker::Deadline)).and_then(|d| d.parse().ok())
}

/// Whether a transfer's checkpoint is there for turn N, run or not.
pub(crate) fn transferring(job: &Job, n: &str) -> bool {
    job.turn_marker(n, TurnMarker::RetryMigration).exists()
}

/// Turn N's transfer checkpoint, as written.
pub(crate) fn checkpoint(job: &Job, n: &str) -> io::Result<Vec<u8>> {
    crate::state_reads::bytes(&job.turn_marker(n, TurnMarker::RetryMigration))
}

/// Whether it was made to run --unstick.
pub(crate) fn unstuck(job: &Job) -> bool {
    job.marker(Marker::Unstick).exists()
}

/// Whether its await is there, empty or not.
pub(crate) fn has_await(job: &Job) -> bool {
    job.marker(Marker::Await).exists()
}

/// Whether its await names anything.
pub(crate) fn awaiting(job: &Job) -> bool {
    filled(&job.marker(Marker::Await))
}

/// Its await's text, as written.
pub(crate) fn await_text(job: &Job) -> Option<String> {
    read(&job.marker(Marker::Await))
}

/// [`await_text`]'s bytes, strictly: an error when it's there and can't be
/// read.
pub(crate) fn await_checked(job: &Job) -> Result<Option<Vec<u8>>, Unread> {
    checked(job.marker(Marker::Await))
}

/// When its await was last written, in nanoseconds.
pub(crate) fn await_written(job: &Job) -> Option<i64> {
    crate::mtime_nanos(&job.marker(Marker::Await))
}

/// Whether its latest turn ended with something left open, read strictly
/// (an error when a marker that would say can't be read): it ended other
/// than done, or done without saying what comes next, and wasn't stopped.
/// What a job the policy denies keeps from the watchdog that its unit
/// doesn't show.
pub(crate) fn left_open(job: &Job) -> io::Result<bool> {
    let text = |r: Result<Option<Vec<u8>>, Unread>| -> io::Result<Option<String>> {
        Ok(r.map_err(Unread::error)?.map(|b| {
            String::from_utf8_lossy(&b)
                .trim_end_matches('\n')
                .to_owned()
        }))
    };
    let Some(n) = text(checked(job.marker(Marker::Turns)))?.filter(|n| valid_turn(n)) else {
        return Ok(false);
    };
    let outcome = text(outcome_checked(job, &n))?;
    let end = text(end_checked(job, &n))?;
    let open = outcome.as_deref() != Some("done") || end.as_deref() == Some("undeclared");
    Ok(open && !job.turn_marker(&n, TurnMarker::Stopped).try_exists()?)
}
