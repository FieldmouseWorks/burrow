//! The hold's words when it's Claude Code's login that's gone, not its
//! usage ([`crate::engine::claude::LOGGED_OUT`]): it resets at no time, only
//! when someone logs in again, so the owner is told to, and the hold is a
//! try at a time (usage.rs UNSAID) until a run answers.
//!
//! And whose runs it holds. A login is a credential, so it holds only the
//! runs that share it: a sandbox runs on its own token
//! (burrow.agents.claudeTokenFile), not this machine's login, so a sandbox's
//! expired token holds no turn here ([`holds_machine`]), and this machine's
//! expired login holds no sandbox's turn ([`Spent::holds_run`]): it still
//! takes a slot ([`out_for`]), routes as if nothing held it, and its answer
//! proves nothing of the login here. A limit is the plan's, which every
//! token shares, so it holds them all, as the owner's pause does.

use super::{CLOSED, Spent, out, policy, when};
use crate::config::Config;

/// Whether the hold recorded as saying `said` is an expired login's.
pub(crate) fn is(said: &str) -> bool {
    said == crate::engine::claude::LOGGED_OUT
}

impl Spent {
    /// Whether it holds a run in a sandbox (`sandboxed`), or one here.
    pub fn holds_run(&self, sandboxed: bool) -> bool {
        !sandboxed || self.owner || !is(&self.said)
    }
}

/// The hold a run in a sandbox (`sandboxed`), or one here, waits on now:
/// for what admits or routes one run, where [`out`] is the machine's.
pub(crate) fn out_for(cfg: &Config, sandboxed: bool) -> Option<Spent> {
    out(cfg).filter(|s| s.holds_run(sandboxed))
}

/// Whether job `id` runs in a sandbox, as its starter's request says (the
/// job's own files don't count): one that can't be read is taken for one
/// here, which every hold holds.
pub(crate) fn sandboxed(cfg: &Config, id: &str) -> bool {
    crate::job::Request::peek(&crate::job::Job::new(cfg, id)).is_ok_and(|r| r.agent.is_some())
}

/// Whether a turn refused on `said` holds the machine: a sandbox's
/// expired token holds no turn here, and logging in here doesn't renew it,
/// so it ends that turn alone ([`SANDBOX`]).
pub(crate) fn holds_machine(said: &str, sandboxed: bool) -> bool {
    !(sandboxed && is(said))
}

/// What a turn refused on `said` is told when it runs again, before its
/// prompt: what it was refused for, and that it's back.
pub(crate) fn resumed(cfg: &Config, said: &str) -> String {
    if is(said) {
        return RESUMED.to_owned();
    }
    format!("{}\n\n{said}. {}", crate::turn::SPENT, policy(cfg))
}

const LOG_IN: &str = "run `claude` as the user proj runs as, then `/login`";

pub(super) fn line(until: i64) -> String {
    format!(
        "Claude Code's login here expired: no Claude turn here starts until someone logs in again ({LOG_IN}), though a sandbox's, on its own token, still does; the next try is at {}",
        when(until)
    )
}

pub(super) const TITLE: &str = "Claude Code's login expired: log in again on this machine";

pub(super) fn body(s: &Spent) -> String {
    format!(
        "Since {}, Claude Code can't authenticate here, so it refuses every Claude job that runs here. Job {} was the first.\n\n\
         Log in again on this machine: {LOG_IN}.\n\n\
         proj starts no Claude turn here meanwhile, and tries one again at {}; a sandbox's (proj run --vm) runs on its own token, and still starts. \
         The turns that were refused wait in line and run again once it's back, by themselves, \
         and the stall watchdog holds what waits on them. \
         This closes once a job here runs again.",
        when(s.at),
        s.job,
        when(s.until),
    )
}

/// The owner's notice's close: what's back.
pub(super) fn closed(s: &Spent) -> &'static str {
    if !s.owner && is(&s.said) {
        "Claude Code is logged in again, and proj's jobs are running."
    } else {
        CLOSED
    }
}

/// What a sandbox's turn refused for it ends saying: its token, not this
/// machine's login, is what's gone ([`holds_machine`]).
pub(crate) const SANDBOX: &str = "Claude Code couldn't authenticate in the sandbox: its token (burrow.agents.claudeTokenFile, from `claude setup-token`) expired or was revoked; make a new one and run the job again";

/// What a run refused for it is told in the next, once it's back, before
/// the turn's prompt.
const RESUMED: &str = "Your last run was refused before it finished: Claude Code's login on this machine had expired, and every Claude job here waited until someone logged in again. It's back now. Carry on from where you left off: see what had already finished before you run anything again. What you were asked this turn:";

#[cfg(test)]
mod tests;
