//! `proj handoff`: a waiting turn given up for a new one.

use std::fs;

use super::{end, put};
use crate::config::Config;
use crate::job::Job;
use crate::turn::handoff::Fate;
use crate::{Fail, io_fail, read, scratch};

/// `proj handoff ID`: the job's waiting turn is given up for a new one, which
/// resolves its engine afresh (the old turn's identity is left as it was), in
/// the same scratch, so the branch and the files the turn left unfinished are
/// where they were. Unlike `proj stop` it keeps the job's scratch, its question
/// and its awaits (the new turn takes the waits and the pending question
/// recommendation over), and answers nothing. Only a turn that still waits for
/// a slot can be handed off (a running one owns its files).
///
/// The job's HANDOFF file names the turn being ended, from the first step to
/// the last, and the rest is read from the job: that turn's stop is no end of
/// the job meanwhile ([`Job::stopped_for_good`]), and asking again after any
/// interruption does what is left. While the job's turn count is still the
/// named turn's, the stop is finished and the new turn started; once it is past
/// it, the new turn was accepted and is adopted, never started a second time.
pub fn handoff(cfg: &Config, id: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    if !job.dir.is_dir() {
        return Err(Fail::new(1, format!("no job {id}")));
    }
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    let Some(mut n) = job.turns() else {
        return Err(Fail::new(1, format!("proj: {id} has no turn to hand off")));
    };
    let began = read(&job.path(scratch::HANDOFF)).map(|t| t.trim().to_owned());
    if let Some(from) = began.as_ref().filter(|from| **from != n) {
        // The turn count moved on, which says only that a successor was
        // numbered. It is adopted once it was accepted; one that wasn't is
        // given up and started again.
        match crate::turn::handoff::fate(cfg, &job, &n) {
            Fate::Accepted => {
                settle_handoff(&job)?;
                put("handed off\n");
                return Ok(());
            }
            Fate::Unaccepted => {
                crate::turn::handoff::abandon(&job, from, &n).map_err(|e| Fail::new(1, e))?;
                n = from.clone();
            }
            Fate::Unknown => {
                return Err(Fail::new(
                    1,
                    format!("proj: can't tell whether {id}'s new turn {n} was accepted; try again"),
                ));
            }
        }
    }
    end(
        cfg,
        &job,
        true,
        if began.is_some() {
            Handoff::Resume
        } else {
            Handoff::Begin
        },
    )?;
    let asked = read(&job.path(&format!("prompt.{n}"))).unwrap_or_default();
    let msg = format!(
        "Your last run waited for capacity and was handed to a new run before it finished. Its work (the branch, changed and new files) is in the worktree just as it left it: see what had already finished before you run anything again, and carry on. What you were asked:\n\n{asked}"
    );
    crate::turn::start_handoff(cfg, id, &msg).map_err(|e| Fail::new(1, e))?;
    settle_handoff(&job)?;
    put("handed off\n");
    Ok(())
}

/// A handoff whose new turn is accepted is over: the turn it ended keeps a
/// marker nothing reads as a stop, and the job's own scratch rules apply again.
/// Either step failing is the handoff's failure: its protection stays.
pub(super) fn settle_handoff(job: &Job) -> Result<(), Fail> {
    let Some(from) = read(&job.path(scratch::HANDOFF)).map(|t| t.trim().to_owned()) else {
        return Ok(());
    };
    if job.turns().as_deref() != Some(from.as_str()) {
        mark_handed_off(job, &from)?;
        retire_handoff(job)?;
    }
    Ok(())
}

/// The turn a handoff ended, marked as handed off rather than stopped.
pub(super) fn mark_handed_off(job: &Job, from: &str) -> Result<(), Fail> {
    match fs::rename(job.turn(from, "stopped"), job.turn(from, "handed-off")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(io_fail(&job.turn(from, "stopped"), e))
        }
        _ => Ok(()),
    }
}

pub(super) fn retire_handoff(job: &Job) -> Result<(), Fail> {
    match fs::remove_file(job.path(scratch::HANDOFF)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(io_fail(&job.path(scratch::HANDOFF), e))
        }
        _ => Ok(()),
    }
}

/// What an ending of a turn is for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Handoff {
    /// `proj stop` or `withdraw`: the job's end.
    No,
    /// A new turn follows: the scratch, the question and the awaits stay.
    Begin,
    /// The same, after one that was interrupted: the turn is no longer looked
    /// at for whether it waits, it is finished stopping.
    Resume,
}
