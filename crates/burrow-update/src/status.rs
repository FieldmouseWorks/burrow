//! What a run did: said, and kept in status.json, which `HOST-update status`
//! reads back and burrow-dispatch watches.

use std::fs;
use std::path::Path;
use std::process::Command;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::Config;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Live and confirmed.
    Ok,
    /// It held off, for something it waits on.
    Held,
    /// Something broke, which is its own to fix (a Claude job is on it).
    Failed,
    /// Nothing to do.
    Current,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Held => "held",
            Outcome::Failed => "failed",
            Outcome::Current => "current",
        }
    }
}

/// What a run did, said and kept: when, how it ended, what happened, the
/// system that runs, and its open PR's number when a commit newer than this
/// run's waits on that landing (`behind`, which burrow-dispatch gives
/// longer; never on a failure, which is the run's own to fix). Kept whole
/// or not at all.
pub fn note(cfg: &Config, outcome: Outcome, message: &str, behind: Option<u64>) {
    println!("{message}");
    let mut status = json!({
        "time": now(),
        "result": outcome.word(),
        "message": message,
        "running": running(cfg),
    });
    if let Some(pr) = behind.filter(|_| outcome != Outcome::Failed) {
        status["behind"] = pr.into();
    }
    let path = cfg.state.join("status.json");
    let new = cfg.state.join("status.json.new");
    if let Err(e) =
        fs::write(&new, status.to_string() + "\n").and_then(|()| fs::rename(&new, &path))
    {
        eprintln!("{}: {}: {e}", cfg.update, path.display());
    }
}

/// The system that runs, as a store path: nothing when it can't be read.
pub fn running(cfg: &Config) -> String {
    fs::canonicalize(&cfg.current_system)
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// `HOST-update status`: what the last run did, how long it has held for
/// a system that isn't main's, the fix jobs it has handed over, the trial
/// boot's state and when the timers fire next.
pub fn status(cfg: &Config) -> String {
    let mut out = String::new();
    let last: Option<Value> = fs::read(cfg.state.join("status.json"))
        .ok()
        .and_then(|t| serde_json::from_slice(&t).ok());
    match last {
        Some(s) => {
            out += &format!(
                "last run {}: {}: {}\n",
                text(&s["time"]),
                text(&s["result"]),
                text(&s["message"])
            )
        }
        None => out += "no run yet\n",
    }
    if let Some(since) = cfg
        .recorded("held-since")
        .parse::<i64>()
        .ok()
        .and_then(|s| Timestamp::from_second(s).ok())
    {
        out += &format!("held since {}\n", iso(since));
    }
    let mut jobs: Vec<_> = fs::read_dir(cfg.state.join("jobs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    jobs.sort();
    for job in jobs {
        let id = fs::read_to_string(&job).unwrap_or_default();
        out += &format!(
            "fix job for {}: {}\n",
            name(&job),
            id.trim_end_matches('\n')
        );
    }
    if let Some(boot) = &cfg.tools.boot
        && let Ok(o) = Command::new(boot).arg("status").output()
    {
        out += &String::from_utf8_lossy(&o.stdout);
    }
    if let Ok(o) = Command::new(&cfg.tools.systemctl)
        .arg("list-timers")
        .args(&cfg.timers)
        .arg("--no-pager")
        .output()
    {
        let timers = String::from_utf8_lossy(&o.stdout);
        for line in timers.lines().skip(1).take(2) {
            out += line;
            out += "\n";
        }
    }
    out
}

/// A JSON value as jq -r prints it.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// A file's name, for what it keeps.
pub fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Now, as `date -Is` says it.
pub fn now() -> String {
    iso(Timestamp::now())
}

fn iso(t: Timestamp) -> String {
    t.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Now, in seconds since the epoch, as `date +%s` says it.
pub fn epoch() -> i64 {
    Timestamp::now().as_second()
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
