//! A run of a turn being watched ([`Run::watch`]) until it's over, and its
//! [`Clock`]: the turn's deadline, its stream going quiet, and its wait in
//! line.

use std::path::Path;
use std::process::{Child, ExitStatus};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use rustix::process::{Pid, Signal, kill_process};

use crate::config::Config;
use crate::engine::Said;
use crate::job::Job;
use crate::waits::{rewatch_later, waiting};
use crate::{mtime_nanos, now_nanos, read};

/// A turn's deadline as it stands, in nanoseconds; None for none.
pub(super) fn deadline(job: &Job, n: &str) -> Option<i64> {
    read(&job.turn(n, "deadline")).and_then(|d| d.parse().ok())
}

/// The file's mtime moved on to now, made if it isn't there.
pub(super) fn touch(path: &Path) {
    if let Ok(f) = crate::state_reads::append(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// A run being watched.
pub(super) struct Run<'a> {
    pub(super) cfg: &'a Config,
    pub(super) job: &'a Job<'a>,
    pub(super) n: &'a str,
    pub(super) waitdir: &'a Path,
    /// Nanoseconds its stream may say nothing before it's stuck; 0 for no limit.
    pub(super) quiet: i64,
}

impl Run<'_> {
    /// Until the run is over: its exit status, and whether its answer cut
    /// it short. The answer ends the turn: a few seconds to exit by itself,
    /// then Claude and whatever it left in the background are stopped (the
    /// unit takes the rest when the turn ends), and the slot goes with them.
    /// Meanwhile its clock is read once a second, sooner when its deadline
    /// is closer ([`Clock`]).
    pub(super) fn watch(&self, mut child: Child, from: u64) -> (ExitStatus, bool) {
        let pid = Pid::from_child(&child);
        let mut said = Said::new(self.job, self.n, from);
        let mut answered: Option<Instant> = None;
        let mut cut = false;
        let mut clock = Clock {
            last: now_nanos(),
            was: None,
            done: false,
            next: Instant::now(),
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => (),
                Err(_) => match child.wait() {
                    Ok(status) => break status,
                    Err(_) => break ExitStatus::default(),
                },
            }
            if answered.is_none() || !said.named {
                said.look(false);
            }
            if answered.is_none() && said.answered {
                answered = Some(Instant::now());
            }
            if !cut && answered.is_some_and(|t| t.elapsed() >= Duration::from_secs(10)) {
                cut = true;
                stop(pid);
            }
            if !clock.done && Instant::now() >= clock.next {
                clock.tick(self, pid);
            }
            let poll = Instant::now() + Duration::from_millis(100);
            let until = if clock.done {
                poll
            } else {
                poll.min(clock.next)
            };
            sleep(until.saturating_duration_since(Instant::now()));
        };
        // What it said last, before it went, can be what names its session.
        if !said.named {
            said.look(true);
        }
        clock.finish(self);
        (status, cut)
    }
}

/// A run's clock: it stops the run once the turn's deadline in N.deadline
/// has passed (N.late), or once its stream has said nothing for the job's
/// --idle (N.quiet), and keeps the turn's wait in line (N.wait, from
/// [`waiting`]) for the watchdog, telling it as one starts or ends. While
/// one of its commands waits in line, the deadline moves on with the clock:
/// waiting its turn for something shared isn't being stuck, and the
/// watchdog holds a wait to stallAfterMinutes. Telling the watchdog goes on
/// in the background, never waited for: its lock or a slow kick holds up
/// neither the clock nor the end of the run.
struct Clock {
    /// When it was last read, in nanoseconds.
    last: i64,
    /// The wait in line it last saw.
    was: Option<String>,
    /// It stopped the run, or the run is over.
    done: bool,
    /// When it's read next.
    next: Instant,
}

impl Clock {
    fn tick(&mut self, run: &Run, pid: Pid) {
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
    fn finish(&mut self, run: &Run) {
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

/// Stop a run: TERM to its timeout(1), which passes it on to all of it.
pub(super) fn stop(pid: Pid) {
    let _ = kill_process(pid, Signal::TERM);
}
