//! The deliveries the service has taken and not yet answered. A stop
//! (SIGTERM: a switch, a new secret) answers those and takes no more, so
//! what comes meanwhile waits on systemd's socket for the next process.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

/// How long a stop waits for them: GitHub gives up on an answer after ten
/// seconds, so one that takes longer is lost to it anyway.
pub const DRAIN: Duration = Duration::from_secs(10);

#[derive(Default)]
struct State {
    stopping: bool,
    unanswered: usize,
}

#[derive(Clone, Default)]
pub struct Taken(Arc<(Mutex<State>, Condvar)>);

/// One taken, until it's dropped: answered, or given up on.
pub struct Unanswered(Taken);

impl Taken {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take what `accept` gets, which mustn't wait: a stop waits for it, so
    /// none is taken off the socket that a stop doesn't know of. Once
    /// stopping it never returns, as the listener's thread mustn't end.
    pub fn take<T>(&self, accept: impl FnOnce() -> io::Result<T>) -> io::Result<(T, Unanswered)> {
        let mut state = self.state();
        if state.stopping {
            drop(state);
            loop {
                thread::park();
            }
        }
        let got = accept()?;
        state.unanswered += 1;
        Ok((got, Unanswered(self.clone())))
    }

    /// Take no more, and wait up to `within` for each taken to be answered:
    /// whether they all were.
    pub fn stop(&self, within: Duration) -> bool {
        let mut state = self.state();
        state.stopping = true;
        let (_state, waited) = self
            .0
            .1
            .wait_timeout_while(state, within, |s| s.unanswered > 0)
            .unwrap_or_else(PoisonError::into_inner);
        !waited.timed_out()
    }
}

impl Drop for Unanswered {
    fn drop(&mut self) {
        self.0.state().unanswered -= 1;
        self.0.0.1.notify_all();
    }
}
