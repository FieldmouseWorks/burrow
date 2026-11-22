//! What a run has to work in (`work_limit`, its unit's TimeoutStartSec),
//! which waiting for a heavy slot doesn't use up. systemd keeps the time
//! and ends a run past it; the run only hands back what it waited, with
//! sd_notify(3)'s EXTEND_TIMEOUT_USEC, which puts the end that long from
//! now and never any sooner. So while it waits, and as it gets its slot,
//! it asks for what it had left when the wait began. A run that stops
//! asking is ended like any other. Starting over from a main that moved
//! (follow.rs) is the same run: its waits come back, its work doesn't.

use std::cell::Cell;
use std::io::{BufRead as _, BufReader, Read};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::Config;
use crate::follow::IN_SLOT;

/// The longest a waiting run goes without asking again.
const BEAT: Duration = Duration::from_secs(60);
/// And the shortest, for one with next to nothing left.
const SHORTEST: Duration = Duration::from_millis(100);

pub(crate) struct Limit<'a> {
    cfg: &'a Config,
    began: Instant,
    waited: Cell<Duration>,
}

impl<'a> Limit<'a> {
    /// The limit of a run that begins now.
    pub(crate) fn begin(cfg: &'a Config) -> Limit<'a> {
        Limit {
            cfg,
            began: Instant::now(),
            waited: Cell::new(Duration::ZERO),
        }
    }

    /// What's left to work in at `now`, outside a wait.
    fn left(&self, now: Instant) -> Duration {
        (Duration::from_secs(self.cfg.work_limit) + self.waited.get())
            .saturating_sub(now.duration_since(self.began))
    }

    /// Waits until `said`, a VM test's stdout through heavy, says it's in
    /// its slot or ends without one, and gives that time back.
    pub(crate) fn wait(&self, said: impl Read + Send + 'static) {
        let since = Instant::now();
        let left = self.left(since);
        let (done, slot) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(said).lines() {
                if line.map_or(true, |l| l == IN_SLOT) {
                    break;
                }
            }
            let _ = done.send(());
        });
        // Asked again before half of what's left is gone, so in time.
        let beat = (left / 2).clamp(SHORTEST, BEAT);
        loop {
            self.extend(left);
            if slot.recv_timeout(beat) != Err(RecvTimeoutError::Timeout) {
                break;
            }
        }
        self.waited.set(self.waited.get() + since.elapsed());
        self.extend(self.left(Instant::now()));
    }

    /// Asks systemd not to end the run for `left` from now. A run by hand
    /// has no unit to ask, and no limit.
    fn extend(&self, left: Duration) {
        let Some(socket) = &self.cfg.notify else {
            return;
        };
        let asked = format!("EXTEND_TIMEOUT_USEC={}", left.as_micros());
        if let Err(e) = burrow_systemd::notify(socket, &asked) {
            eprintln!(
                "{}: couldn't hand back its wait for a slot to systemd: {e}",
                self.cfg.update
            );
        }
    }
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
