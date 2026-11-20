//! The `claude-api` engine: the same Claude Code as `claude`, started on the
//! API credit's key instead of the subscription's login, and only while the
//! credit has room.
//!
//! Credentials. Claude Code takes an API key over its login, and a key in
//! the environment would bill every turn that inherits it to the API; so
//! every `claude` run is started with no API credential at all, and a
//! `claude-api` run with no login token and the key on a descriptor
//! (`CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`), opened from its file
//! (burrow.proj.claudeApi.keyFile) only for that run: the key is in no
//! variable, argument or settings file, and the commands Claude Code runs
//! inherit neither it nor the descriptor ([`credentials`]). The login's
//! file stays where it is, for the sessions both engines share
//! ([`super::Engine::conversation`]); Claude Code reads no login when it has
//! a key. A sandbox is given no key, so the engine never runs in one.
//!
//! Admission. Every run of a `claude-api` turn, its first and each retry,
//! starts only through [`admit`]: under one lock for the machine, the
//! credit's meter (burrow_probe::credit) is read, and the run is given what
//! it has room for as its `--max-budget-usd`, which its marker records
//! before the lock goes. The meter counts a run whose cost isn't recorded
//! yet at that budget, so runs going at once never spend the same room
//! twice. Claude Code stops a run past its budget after the call that took
//! it there, so a run can go one call over: the setting's reserve is what
//! absorbs that, and the Console's spend limit is the backstop past it.
//!
//! Refusals. Nothing about the credit is the plan's: no refusal here
//! records the plan's usage hold, and none of the plan's holds stops this
//! engine (usage/login.rs). A launch the credit can't admit, or a run the
//! API refused (its key, its credit, its budget), ends its turn with words
//! of its own ([`Refused`]).

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::{Engine, is_error};
use crate::config::Config;

/// What the turn records when its engine is refused: the credit's own
/// refusal, never the plan's hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} refused: {}", Engine::ClaudeApi.name(), self.0)
    }
}

impl std::error::Error for Refused {}

impl Refused {
    pub(crate) fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
    /// As the error a launch returns.
    pub fn io(self) -> io::Error {
        io::Error::other(self)
    }
    /// The refusal a launch's error carries, if it's one.
    pub fn of(e: &io::Error) -> Option<&Refused> {
        e.get_ref().and_then(|e| e.downcast_ref::<Refused>())
    }
}

/// The variables that carry an API credential, which bill the API: none
/// reaches a `claude` run, and a `claude-api` run gets only its own key.
pub const API_CREDENTIALS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];
/// The variables that carry the subscription's login: none reaches a
/// `claude-api` run.
pub const LOGIN_CREDENTIALS: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
];
/// Where Claude Code reads a key from a descriptor.
const KEY_FD: &str = "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR";

/// The key's file, opened and checked: refused when none is configured, or
/// it can't be read, or holds nothing. The key itself is never in an error.
pub fn key(cfg: &Config) -> Result<File, Refused> {
    let path = cfg
        .claude_api_key_file
        .as_deref()
        .ok_or_else(|| Refused::new("no API key is configured (burrow.proj.claudeApi.keyFile)"))?;
    let unreadable = |e: io::Error| {
        Refused::new(format!(
            "its API key in {} can't be read: {}",
            path.display(),
            e.kind()
        ))
    };
    let mut file = File::open(path).map_err(unreadable)?;
    let mut key = String::new();
    file.read_to_string(&mut key).map_err(unreadable)?;
    if key.trim().is_empty() {
        return Err(Refused::new(format!(
            "its API key file {} is empty",
            path.display()
        )));
    }
    file.rewind().map_err(unreadable)?;
    Ok(file)
}

/// The credentials `engine`'s Claude Code starts with, set on `c` after
/// everything else it inherits: for `claude`, no API credential; for
/// `claude-api`, no login and the key on a descriptor, open in the file
/// returned, which must outlive the start.
pub fn credentials(c: &mut Command, engine: Engine, cfg: &Config) -> Result<Option<File>, Refused> {
    for name in API_CREDENTIALS {
        c.env_remove(name);
    }
    match engine {
        Engine::Claude => Ok(None),
        Engine::ClaudeApi => {
            for name in LOGIN_CREDENTIALS {
                c.env_remove(name);
            }
            let file = key(cfg)?;
            // Opened close-on-exec, as Rust opens every file: this one
            // alone is kept open across the exec, for Claude Code to read.
            rustix::io::fcntl_setfd(&file, rustix::io::FdFlags::empty())
                .map_err(|e| Refused::new(format!("its API key can't be handed on: {e}")))?;
            c.env(KEY_FD, file.as_raw_fd().to_string());
            Ok(Some(file))
        }
    }
}

/// A run the credit admitted: what it may spend, in micro-dollars, and the
/// lock that keeps every other admission out until its marker is written.
#[derive(Debug)]
pub struct Admission {
    pub budget: i64,
    _lock: File,
}

impl Admission {
    /// Its budget as `--max-budget-usd` takes it.
    pub fn flag(&self) -> [String; 2] {
        ["--max-budget-usd".into(), usd(self.budget)]
    }
}

/// Dollars to the micro-dollar, as Claude Code takes them.
fn usd(micros: i64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// Where admissions take turns, in jobs.
const LOCK: &str = ".credit.lock";

/// Admit a run now, or refuse it: no credit set, a meter that can't be
/// read, or no room in it (runs still going count at their budgets). The
/// lock is held until the [`Admission`] is dropped, which the caller does
/// once the run's marker, with its budget, is written.
pub fn admit(cfg: &Config, now: i64) -> Result<Admission, Refused> {
    let settings = cfg
        .settings
        .as_ref()
        .ok_or_else(|| Refused::new("no settings to read its credit from"))?;
    let path = cfg.jobs.join(LOCK);
    let lock = File::create(&path)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| Refused::new(format!("{}: {e}", path.display())))?;
    let meter = meter(cfg, &settings.credit_file(), now)?;
    let budget = meter.headroom();
    if budget <= 0 {
        return Err(Refused::new(format!(
            "its credit has no room: {}",
            meter.short().unwrap_or_else(|| "none left".into())
        )));
    }
    Ok(Admission {
        budget,
        _lock: lock,
    })
}

fn meter(cfg: &Config, credit: &Path, now: i64) -> Result<burrow_settings::credit::Meter, Refused> {
    burrow_probe::credit::meter(credit, Some(&cfg.jobs), now)
        .map_err(|e| Refused::new(format!("its credit can't be metered: {e}")))
}

/// Whether the credit has room now, as an observer reads it (the drain's
/// look): no lock, nothing admitted.
pub fn room(cfg: &Config) -> bool {
    cfg.settings
        .as_ref()
        .is_some_and(|s| meter(cfg, &s.credit_file(), crate::now()).is_ok_and(|m| m.room()))
}

/// Whether the credit is short at `pct` now, as a route reads it
/// ([`super::route`]): no room (none set, a meter that can't be read, the
/// reserve or today's allowance reached), or `pct` percent of its cycle's
/// budget spent.
pub fn short(cfg: &Config, pct: f64, now: i64) -> bool {
    let Some(settings) = cfg.settings.as_ref() else {
        return true;
    };
    meter(cfg, &settings.credit_file(), now).map_or(true, |m| {
        !m.room() || m.spent as f64 * 100.0 >= pct * m.cycle.budget as f64
    })
}

/// When the clock alone may give the credit room again, as an observer
/// reads it ([`burrow_settings::credit::Meter::opens`]): what the drain books
/// a pass for when it has none. None with no credit set or a meter that
/// can't be read (fixing either is an event of its own), or while only a
/// run going holds it (its turn's end kicks the drain).
pub fn opens(cfg: &Config) -> Option<i64> {
    let settings = cfg.settings.as_ref()?;
    meter(cfg, &settings.credit_file(), crate::now())
        .ok()?
        .opens()
}

/// Why a finished run was refused, when the API refused it (its key, its
/// credit or its budget): `last` is its result, `records` its stream. Any
/// other error is the turn's own, ended like any other.
pub fn refusal(last: &Value, records: impl Iterator<Item = Value>) -> Option<Refused> {
    if !is_error(last) {
        return None;
    }
    if last["subtype"] == "error_max_budget_usd" {
        return Some(Refused::new(
            "the run spent the budget its admission gave it",
        ));
    }
    let said = last["result"].as_str().unwrap_or_default().trim();
    let lower = said.to_lowercase();
    let mut records = records;
    // With the descriptor's variable set, Claude Code reads no login: a key
    // it can't read ends the run "Not logged in", having sent nothing. A
    // run that says it started on anything but its key is refused too.
    if lower.starts_with("invalid api key")
        || lower.starts_with("failed to authenticate")
        || lower.starts_with("not logged in")
        || records.any(|v| {
            v["error"] == "authentication_failed"
                || (v["subtype"] == "init"
                    && v["apiKeySource"].is_string()
                    && v["apiKeySource"] != "ANTHROPIC_API_KEY")
        })
    {
        return Some(Refused::new("the API didn't take its key"));
    }
    if last["api_error"] == "usage_limit_reached"
        || ["credit balance", "spend limit", "usage limit", "billing"]
            .iter()
            .any(|w| lower.contains(w))
    {
        return Some(Refused::new(
            "the API refused it for its credit or spend limit",
        ));
    }
    None
}

/// [`refusal`], from the run's records in `path` past `from`.
pub fn refusal_file(last: &Value, path: &Path, from: u64) -> Option<Refused> {
    let records = super::stream::lines(path, from)
        .ok()?
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok());
    refusal(last, records)
}

#[cfg(test)]
mod tests;
