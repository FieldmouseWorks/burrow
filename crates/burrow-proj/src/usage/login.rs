//! Per-engine run identity (`Run`), expired-login hold scoping, and login/failover
//! wording.
//!
//! A hold belongs to one engine (or `*` for a global owner pause): a quota or
//! login refusal on engine A never blocks engine B. Within one engine, a host
//! login expiry holds only host runs of that engine, leaving sandbox runs (which
//! carry their own `credentialFile`) free to run; quota limits and owner pauses
//! hold all runs of that engine.

use super::store::{ALL_ENGINES, DEFAULT_ENGINE, Spent};
use super::{out_for as usage_out_for, policy, when};
use crate::config::Config;

/// Whether the refusal text `said` matches Claude's expired-login sentinel.
pub(crate) fn is(said: &str) -> bool {
    said == burrow_engine_anthropic::claude::LOGGED_OUT
}

/// A run a hold may apply to: which engine it runs on, and whether it runs in
/// a sandbox (with its own credential rather than this machine's host login).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub engine: String,
    pub sandboxed: bool,
}

impl Default for Run {
    fn default() -> Self {
        Self::new(DEFAULT_ENGINE, false)
    }
}

impl Run {
    pub fn new(engine: impl Into<String>, sandboxed: bool) -> Self {
        Self {
            engine: engine.into(),
            sandboxed,
        }
    }

    /// A run on the default (`claude`) engine, in a sandbox or on the host.
    pub(crate) fn plan(sandboxed: bool) -> Self {
        Self::new(DEFAULT_ENGINE, sandboxed)
    }
}

impl Spent {
    /// Whether this hold blocks `run`:
    /// - a global owner pause (`*`) blocks every engine;
    /// - a hold on another engine never blocks `run`;
    /// - a per-engine owner pause or usage limit blocks every run of that engine;
    /// - an expired host login blocks only host (non-sandboxed) runs of that engine.
    pub fn holds_run(&self, run: &Run) -> bool {
        let key = if self.engine.is_empty() {
            DEFAULT_ENGINE
        } else {
            self.engine.as_str()
        };
        if key == ALL_ENGINES {
            return true;
        }
        if key != run.engine {
            return false;
        }
        self.owner || !run.sandboxed || !self.is_login()
    }
}

/// The active hold blocking `run` now, if any.
pub(crate) fn out_for(cfg: &Config, run: &Run) -> Option<Spent> {
    usage_out_for(cfg, run)
}

/// The run job `id`'s current turn represents, read strictly from host-owned
/// execution identity and request records (never guest-writable job files).
pub(crate) fn run_of(cfg: &Config, id: &str) -> Run {
    let job = crate::job::Job::new(cfg, id);
    let engine = job
        .turns()
        .and_then(|n| {
            crate::retry::Identity::load(&job, &n)
                .ok()
                .flatten()
                .map(|i| i.choice.engine.name().to_owned())
        })
        .or_else(|| {
            crate::job::Request::peek(&job)
                .ok()
                .and_then(|r| r.engine.map(|e| e.name().to_owned()))
        })
        .unwrap_or_else(|| DEFAULT_ENGINE.to_owned());
    Run {
        engine,
        sandboxed: sandboxed(cfg, id),
    }
}

/// Whether job `id` runs in a sandbox, from its host-owned request record.
pub(crate) fn sandboxed(cfg: &Config, id: &str) -> bool {
    crate::job::Request::peek(&crate::job::Job::new(cfg, id)).is_ok_and(|r| r.agent.is_some())
}

/// Whether a turn refused on `said` holds the host engine: a sandbox's expired
/// token belongs only to that sandbox, never to this machine's host login.
pub(crate) fn holds_machine(said: &str, sandboxed: bool) -> bool {
    !(sandboxed && is(said))
}

/// Encode an engine reroute (within the same conversation or across conversations).
pub(crate) fn moved_to(engine: &str, same_conversation: bool, said: &str) -> String {
    let prefix = if same_conversation {
        MOVED
    } else {
        MOVED_CROSS
    };
    format!("{prefix}{engine}: {said}")
}

const MOVED: &str = "moved to ";
const MOVED_CROSS: &str = "handed off to ";

/// What a refused turn is told when its retry starts.
pub(crate) fn resumed(cfg: &Config, said: &str) -> String {
    if is(said) {
        return RESUMED.to_owned();
    }
    if let Some((engine, why)) = said.strip_prefix(MOVED).and_then(|m| m.split_once(": ")) {
        return format!(
            "Your last run was refused before it finished ({why}), so this turn carries on on {engine}, the same Claude Code in the same conversation. Carry on from where you left off: see what had already finished before you run anything again. What you were asked this turn:"
        );
    }
    if let Some((engine, why)) = said
        .strip_prefix(MOVED_CROSS)
        .and_then(|m| m.split_once(": "))
    {
        return format!(
            "Your last run was refused before it finished ({why}), so this turn carries on on {engine} in a fresh conversation. Carry on from where you left off: see what had already finished before you run anything again. What you were asked this turn:"
        );
    }
    format!("{}\n\n{said}. {}", crate::turn::SPENT, policy(cfg))
}

const CLAUDE_LOG_IN: &str = "run `claude` as the user proj runs as, then `/login`";

pub(super) fn line_for(engine: &str, until: i64) -> String {
    if engine == DEFAULT_ENGINE {
        format!(
            "Claude Code's login here expired: no Claude turn here starts until someone logs in again ({CLAUDE_LOG_IN}), though a sandbox's, on its own token, still does; the next try is at {}",
            when(until)
        )
    } else {
        format!(
            "{engine}'s login here expired: no {engine} turn here starts until someone logs in again for {engine}, though a sandbox's, on its own credential, still does; the next try is at {}",
            when(until)
        )
    }
}

pub(super) fn title_for(engine: &str) -> String {
    if engine == DEFAULT_ENGINE {
        "Claude Code's login expired: log in again on this machine".to_owned()
    } else {
        format!("{engine}'s login expired: log in again on this machine")
    }
}

pub(super) fn body_for(engine: &str, s: &Spent) -> String {
    if engine == DEFAULT_ENGINE {
        format!(
            "Since {}, Claude Code can't authenticate here, so it refuses every Claude job that runs here. Job {} was the first.\n\n\
             Log in again on this machine: {CLAUDE_LOG_IN}.\n\n\
             proj starts no Claude turn here meanwhile, and tries one again at {}; a sandbox's (proj run --vm) runs on its own token, and still starts. \
             The turns that were refused wait in line and run again once it's back, by themselves, \
             and the stall watchdog holds what waits on them. \
             This closes once a job here runs again.",
            when(s.at),
            s.job,
            when(s.until),
        )
    } else {
        format!(
            "Since {}, {engine} can't authenticate here, so it refuses every {engine} job that runs here. Job {} was the first.\n\n\
             Re-authenticate {engine} on this machine as the user proj runs as.\n\n\
             proj starts no {engine} turn here meanwhile, and tries one again at {}; other engines and sandbox runs still start. \
             The turns that were refused wait in line and run again once it's back, by themselves, \
             and the stall watchdog holds what waits on them. \
             This closes once a {engine} job here runs again.",
            when(s.at),
            s.job,
            when(s.until),
        )
    }
}

pub(super) fn closed_for(engine: &str) -> String {
    if engine == DEFAULT_ENGINE {
        "Claude Code is logged in again, and proj's jobs are running.".to_owned()
    } else {
        format!("{engine} is logged in again, and proj's jobs are running.")
    }
}

/// What a sandbox's turn refused for expired auth ends saying.
pub(crate) const SANDBOX: &str = "Claude Code couldn't authenticate in the sandbox: its token (burrow.agents.engines.claude.credentialFile, from `claude setup-token`) expired or was revoked; make a new one and run the job again";

const RESUMED: &str = "Your last run was refused before it finished: Claude Code's login on this machine had expired, and every Claude job here waited until someone logged in again. It's back now. Carry on from where you left off: see what had already finished before you run anything again. What you were asked this turn:";

#[cfg(test)]
mod tests;
