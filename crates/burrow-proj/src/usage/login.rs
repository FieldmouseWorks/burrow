//! The hold's words when it's Claude Code's login that's gone, not its
//! usage ([`crate::engine::claude::LOGGED_OUT`]): it resets at no time, only
//! when someone logs in again, so the owner is told to, and the hold is a
//! try at a time (usage.rs UNSAID) until a run answers.

use super::{Spent, when};

/// Whether the hold recorded as saying `said` is an expired login's.
pub(super) fn is(said: &str) -> bool {
    said == crate::engine::claude::LOGGED_OUT
}

const LOG_IN: &str = "run `claude` as the user proj runs as, then `/login`";

pub(super) fn line(until: i64) -> String {
    format!(
        "Claude Code's login here expired: no Claude turn starts until someone logs in again ({LOG_IN}); the next try is at {}",
        when(until)
    )
}

pub(super) const TITLE: &str = "Claude Code's login expired: log in again on this machine";

pub(super) fn body(s: &Spent) -> String {
    format!(
        "Since {}, Claude Code can't authenticate here, so it refuses every Claude job. Job {} was the first.\n\n\
         Log in again on this machine: {LOG_IN}.\n\n\
         proj starts no Claude turn meanwhile, and tries one again at {}. The turns that were refused wait in line and run again once it's back, by themselves, \
         and the stall watchdog holds what waits on them. \
         This closes once a job runs again.",
        when(s.at),
        s.job,
        when(s.until),
    )
}

pub(super) const CLOSED: &str = "Claude Code is logged in again, and proj's jobs are running.";

/// What a run refused for it is told in the next, once it's back, before
/// the turn's prompt.
pub(super) const RESUMED: &str = "Your last run was refused before it finished: Claude Code's login on this machine had expired, and every Claude job waited until someone logged in again. It's back now. Carry on from where you left off: see what had already finished before you run anything again. What you were asked this turn:";

#[cfg(test)]
mod tests;
