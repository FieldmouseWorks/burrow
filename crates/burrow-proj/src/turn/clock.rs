//! A run's clock: the turn's deadline, its --idle, and its wait in line.

use std::time::{Duration, Instant};

use rustix::process::Pid;

use super::{Run, deadline, stop, touch};
use crate::waits::{rewatch_later, waiting};
use crate::{mtime_nanos, now_nanos};

/// A run's clock: it stops the run once the turn's deadline in N.deadline
/// has passed (N.late), or once its stream has said nothing for the job's
/// --idle (N.quiet), and keeps the turn's wait in line (N.wait, from
/// [`waiting`]) for the watchdog, telling it as one starts or ends. While
/// one of its commands waits in line, the deadline moves on with the clock:
/// waiting its turn for something shared isn't being stuck, and the
/// watchdog holds a wait to stallAfterMinutes. Telling the watchdog goes on
/// in the background, never waited for: its lock or a slow kick holds up
/// neither the clock nor the end of the run.
pub(super) struct Clock {
    /// When it was last read, in nanoseconds.
    pub(super) last: i64,
    /// The wait in line it last saw.
    pub(super) was: Option<String>,
    /// It stopped the run, or the run is over.
    pub(super) done: bool,
    /// When it's read next.
    pub(super) next: Instant,
}

impl Clock {
    pub(super) fn tick(&mut self, run: &Run, pid: Pid) {
        let (job, n) = (run.job, run.n);
        let now = now_nanos();
        let w = waiting(run.waitdir).map(|(since, what)| format!("{since}\t{what}"));
        let mut dl = deadline(job, n);
        if w.is_some()
            && let Some(d) = dl
        {
            let moved = d + now - self.last;
            let _ = crate::state_reads::write(job.turn(n, "deadline"), format!("{moved}\n"));
            dl = Some(moved);
        }
        self.last = now;
        if w != self.was {
            match &w {
                Some(w) => {
                    let _ = crate::state_reads::write(job.turn(n, "wait"), format!("{w}\n"));
                }
                None => {
                    let _ = crate::state_reads::remove(job.turn(n, "wait"));
                }
            }
            self.was = w.clone();
            rewatch_later(run.cfg, job.id);
        }
        if let Some(d) = dl
            && now >= d
        {
            touch(&job.turn(n, "late"));
            stop(pid);
            self.finish(run);
            return;
        }
        let stream = mtime_nanos(&job.turn(n, "jsonl")).unwrap_or(0);
        if gone_quiet(now, stream, run.quiet) {
            touch(&job.turn(n, "quiet"));
            stop(pid);
            self.finish(run);
            return;
        }
        let mut nap = Duration::from_secs(1);
        if let Some(d) = dl
            && w.is_none()
            && d - now < 1_000_000_000
        {
            nap = Duration::from_nanos((d - now).max(0) as u64);
        }
        self.next = Instant::now() + nap;
    }

    /// Done with: a wait it was keeping is over.
    pub(super) fn finish(&mut self, run: &Run) {
        if self.done {
            return;
        }
        self.done = true;
        if self.was.take().is_some() {
            let _ = crate::state_reads::remove(run.job.turn(run.n, "wait"));
            rewatch_later(run.cfg, run.job.id);
        }
    }
}

/// Whether a stream last written at `stream` has said nothing for all of
/// `quiet` by `now`, all in nanoseconds; a `quiet` of 0 is no --idle.
pub(super) fn gone_quiet(now: i64, stream: i64, quiet: i64) -> bool {
    quiet > 0 && now - stream >= quiet
}
