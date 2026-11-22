//! What a run of HOST-update's unit has to work in (`WORK_LIMIT`), which
//! waiting for a heavy slot doesn't use up. The kernel keeps the time, in a
//! timer of the run's own that kills it at the end, stopped or not, and
//! systemd then ends what it started, as for any main process killed. It
//! can't be the unit's TimeoutStartSec: systemd forgets what's added to
//! that (EXTEND_TIMEOUT_USEC) at each daemon-reload, the run's own switch's
//! included, and counts from the unit's start again. So while a run waits,
//! and as it gets its slot, it sets the timer to what it had left when the
//! wait began, and one that stops doing so is ended like any other.
//! Starting over from a main that moved (follow.rs) is the same run: its
//! waits come back, its work doesn't.

use std::cell::{Cell, RefCell};
use std::io::{BufRead as _, BufReader, Read};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{SigEvent, SigevNotify, Signal};
use nix::sys::time::{TimeSpec, TimeValLike as _};
use nix::sys::timer::{Expiration, Timer, TimerSetTimeFlags};
use nix::time::ClockId;

use crate::follow::IN_SLOT;
use crate::{Broke, Config};

/// The seconds a run has to work in, as its unit says them in the
/// environment. A run by hand hears none, and has no limit.
pub const WORK_LIMIT: &str = "BURROW_UPDATE_WORK_LIMIT";
/// The longest a waiting run goes without setting its timer again.
const BEAT: Duration = Duration::from_secs(60);
/// And the shortest, for one with next to nothing left.
const SHORTEST: Duration = Duration::from_millis(100);

pub(crate) struct Limit {
    began: Instant,
    waited: Cell<Duration>,
    /// What it has, and what ends it then: none by hand.
    kept: Option<(Duration, RefCell<Timer>)>,
}

impl Limit {
    /// The limit of a run that begins now, its time already running.
    pub(crate) fn begin(cfg: &Config) -> Result<Limit, Broke> {
        let kept = match cfg.work_limit {
            Some(work) => {
                let kill = SigevNotify::SigevSignal {
                    signal: Signal::SIGKILL,
                    si_value: 0,
                };
                let timer = Timer::new(ClockId::CLOCK_MONOTONIC, SigEvent::new(kill))
                    .and_then(|mut timer| set(&mut timer, work).map(|()| timer))
                    .map_err(|e| Broke(format!("its time limit's timer: {e}")))?;
                Some((work, RefCell::new(timer)))
            }
            None => None,
        };
        Ok(Limit {
            began: Instant::now(),
            waited: Cell::new(Duration::ZERO),
            kept,
        })
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
        // Set again before half of what's left is gone, so in time.
        let beat = left.map_or(BEAT, |left| (left / 2).clamp(SHORTEST, BEAT));
        loop {
            self.end_in(left);
            if slot.recv_timeout(beat) != Err(RecvTimeoutError::Timeout) {
                break;
            }
        }
        self.waited.set(self.waited.get() + since.elapsed());
        self.end_in(self.left(Instant::now()));
    }

    /// What's left to work in at `now`, outside a wait.
    fn left(&self, now: Instant) -> Option<Duration> {
        let (work, _) = self.kept.as_ref()?;
        Some((*work + self.waited.get()).saturating_sub(now.duration_since(self.began)))
    }

    /// Has the run end `left` from now.
    fn end_in(&self, left: Option<Duration>) {
        if let (Some(left), Some((_, timer))) = (left, &self.kept)
            && let Err(e) = set(&mut timer.borrow_mut(), left)
        {
            eprintln!("couldn't hand back a wait for a slot to the time limit: {e}");
        }
    }
}

/// `timer` set to go off `left` from now. None left is at once: zero would
/// turn it off.
fn set(timer: &mut Timer, left: Duration) -> nix::Result<()> {
    let left = TimeSpec::from_duration(left).max(TimeSpec::nanoseconds(1));
    timer.set(Expiration::OneShot(left), TimerSetTimeFlags::empty())
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
