//! The `claude-api` engine: the same Claude Code as `claude`, started on the
//! API credit's key instead of the subscription's login, and only while the
//! credit has room.
//!
//! Credentials. Claude Code takes an API key over its login, and a key in
//! the environment would bill every turn that inherits it to the API; so
//! every `claude` run is started with no API credential at all, and a
//! `claude-api` run with no login token and the key on a descriptor
//! (`CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`), opened from its file
//! (`burrow.proj.claudeApi.keyFile`) only for that run: the key is in no
//! variable, argument or settings file, and the commands Claude Code runs
//! inherit neither it nor the descriptor ([`credentials`]). The login's
//! file stays where it is, for the sessions both modes share; Claude Code
//! reads no login when it has a key. A sandbox is given no key, so the engine
//! never runs in one.
//!
//! Admission. Every run of a `claude-api` turn, its first and each retry,
//! starts only through [`admit`]: under one lock for the machine, the
//! credit's meter (`burrow_probe::credit`) is read, and the run is given what
//! it has room for as its `--max-budget-usd`, which its marker records
//! before the lock goes. The meter counts a run whose cost isn't recorded
//! yet at that budget, so runs going at once never spend the same room
//! twice. Claude Code stops a run past its budget after the call that took
//! it there, so a run can go one call over: the setting's reserve is what
//! absorbs that, and the Console's spend limit is the backstop past it.
//! A run outside proj's jobs (the plugin's) is admitted through
//! [`reserve`], which writes its marker in the meter's own folder for such
//! runs, and its cost there once it ends.
//!
//! Refusals. Nothing about the credit is the plan's: no refusal here
//! records the plan's usage hold, and none of the plan's holds stops this
//! engine. A launch the credit can't admit, or a run the API refused (its
//! key, its credit, its budget), ends its turn with words of its own
//! ([`Refused`]).

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;

use burrow_probe::credit::{ENGINE, ENGINE_RUNS};
use burrow_process::state_reads::{self, Dir};
use serde_json::Value;

use crate::{Mode, stream};

/// What the turn records when its engine is refused: the credit's own
/// refusal, never the plan's hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} refused: {}", Mode::ClaudeApi.name(), self.0)
    }
}

impl std::error::Error for Refused {}

impl Refused {
    pub fn new(why: impl Into<String>) -> Self {
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
pub fn key(key_file: Option<&Path>) -> Result<File, Refused> {
    let path = key_file
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

/// An unlinked temporary file holding an in-memory API key value (e.g. received
/// via `credentialEnv` inside a sandbox), rewound and ready to hand to Claude
/// Code on `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`. The key itself never appears
/// in an error, argument, or child environment variable.
pub fn key_from_value(value: Option<&str>) -> Result<File, Refused> {
    let raw = value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Refused::new("no API key is available in its credential"))?;
    let mut file = tempfile::tempfile()
        .map_err(|e| Refused::new(format!("its API key file can't be created: {}", e.kind())))?;
    file.write_all(raw.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.rewind())
        .map_err(|e| Refused::new(format!("its API key can't be prepared: {}", e.kind())))?;
    Ok(file)
}

/// The credentials `mode`'s Claude Code starts with, set on `c` after
/// everything else it inherits: for `claude`, no API credential; for
/// `claude-api`, no login and the key on a descriptor, open in the file
/// returned, which must outlive the start.
pub fn credentials(
    c: &mut Command,
    mode: Mode,
    key_file: Option<&Path>,
) -> Result<Option<File>, Refused> {
    prepare_credentials(c, mode, || key(key_file))
}

/// Like [`credentials`], but for `claude-api` takes the key value directly
/// (from `credentialEnv`, or falling back to `key_file` when no env value is
/// present) and passes it to Claude Code solely via an unlinked file descriptor.
pub fn credentials_from_source(
    c: &mut Command,
    mode: Mode,
    key_value: Option<&str>,
    key_file: Option<&Path>,
) -> Result<Option<File>, Refused> {
    prepare_credentials(c, mode, || match key_value {
        Some(v) if !v.trim().is_empty() => key_from_value(Some(v)),
        _ if key_file.is_some() => key(key_file),
        _ => key_from_value(key_value),
    })
}

fn prepare_credentials(
    c: &mut Command,
    mode: Mode,
    open_key: impl FnOnce() -> Result<File, Refused>,
) -> Result<Option<File>, Refused> {
    for name in API_CREDENTIALS {
        c.env_remove(name);
    }
    match mode {
        Mode::Claude => Ok(None),
        Mode::ClaudeApi => {
            for name in LOGIN_CREDENTIALS {
                c.env_remove(name);
            }
            let file = open_key()?;
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
        budget_flag(self.budget)
    }
}

/// A budget in micro-dollars as `--max-budget-usd` takes it.
pub fn budget_flag(micros: i64) -> [String; 2] {
    ["--max-budget-usd".into(), usd(micros)]
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
pub fn admit(credit_file: Option<&Path>, jobs: &Path, now: i64) -> Result<Admission, Refused> {
    let credit = credit_file.ok_or_else(|| Refused::new("no settings to read its credit from"))?;
    let lock = lock(jobs)?;
    let meter = meter(credit, jobs, now)?;
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

/// The machine's admission lock, held until the file is dropped.
fn lock(jobs: &Path) -> Result<File, Refused> {
    let path = jobs.join(LOCK);
    File::create(&path)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| Refused::new(format!("{}: {e}", path.display())))
}

#[derive(Debug)]
enum ReservationTarget {
    EngineRuns { turn: u64 },
    JobStream { path: std::path::PathBuf, from: u64 },
}

/// A run the credit admitted, recorded in either the meter's standalone
/// `.engine-runs` ledger or directly in a proj job's turn stream (`N.jsonl`):
/// its marker, with its budget, is written before the admission lock goes, so
/// the meter holds that budget until [`Reservation::finish`] records its
/// cost, and [`Reservation::abandon`] takes it back for a run that never
/// started.
#[derive(Debug)]
pub struct Reservation {
    /// What the run may spend, in micro-dollars: its `--max-budget-usd`.
    pub budget: i64,
    jobs: std::path::PathBuf,
    target: ReservationTarget,
}

/// Admit a run now, at no more than `cap` when it has one, and record it in
/// the ledger before the lock goes: a run whose marker can't be written is
/// refused, never run unrecorded.
pub fn reserve(
    credit_file: Option<&Path>,
    jobs: &Path,
    now: i64,
    cap: Option<i64>,
) -> Result<Reservation, Refused> {
    let admitted = admit(credit_file, jobs, now)?;
    let budget = cap.map_or(admitted.budget, |cap| admitted.budget.min(cap));
    let fail = |e: io::Error| Refused::new(format!("its run can't be recorded: {e}"));
    let runs = state_reads::directory(&jobs.join(ENGINE_RUNS)).map_err(fail)?;
    let turn = runs
        .names()
        .map_err(fail)?
        .iter()
        .filter_map(|name| name.to_str()?.strip_suffix(".jsonl")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let recorded = (|| {
        runs.create(&format!("{turn}.engine"), format!("{ENGINE}\n").as_bytes())?;
        let mut stream = runs.create_new(&format!("{turn}.jsonl"))?;
        writeln!(
            stream,
            "{}",
            burrow_probe::credit::marker(now, ENGINE, Some(budget))
        )
    })();
    if let Err(e) = recorded {
        let _ = remove_turn(&runs, turn);
        return Err(fail(e));
    }
    drop(admitted);
    Ok(Reservation {
        budget,
        jobs: jobs.to_owned(),
        target: ReservationTarget::EngineRuns { turn },
    })
}

/// Admit a host `proj` job turn run under `.credit.lock` and append its
/// budgeted `burrow.run` marker directly to `stream_path` (`N.jsonl`), avoiding
/// duplicate accounting in `.engine-runs`.
pub fn reserve_in_stream(
    credit_file: Option<&Path>,
    jobs: &Path,
    stream_path: &Path,
    now: i64,
    cap: Option<i64>,
    model: Option<&str>,
) -> Result<Reservation, Refused> {
    let admitted = admit(credit_file, jobs, now)?;
    let budget = cap.map_or(admitted.budget, |cap| admitted.budget.min(cap));
    let fail = |e: io::Error| Refused::new(format!("its run can't be recorded: {e}"));
    let mut stream = state_reads::append(stream_path).map_err(fail)?;
    let from = stream.metadata().map_err(fail)?.len();
    let recorded = writeln!(
        stream,
        "{}",
        burrow_probe::credit::marker_with_model(now, ENGINE, Some(budget), model)
    );
    if let Err(e) = recorded {
        let _ = stream.set_len(from);
        return Err(fail(e));
    }
    drop(admitted);
    Ok(Reservation {
        budget,
        jobs: jobs.to_owned(),
        target: ReservationTarget::JobStream {
            path: stream_path.to_owned(),
            from,
        },
    })
}

fn remove_turn(runs: &Dir, turn: u64) -> io::Result<()> {
    for name in [format!("{turn}.jsonl"), format!("{turn}.engine")] {
        match runs.remove(&name) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

impl Reservation {
    fn runs(&self) -> io::Result<Dir> {
        Dir::open(&self.jobs.join(ENGINE_RUNS))
    }

    /// Take back the reservation of a run that never started, under the
    /// lock, so no admission reads the turn half gone.
    pub fn abandon(self) -> Result<(), Refused> {
        let _lock = lock(&self.jobs)?;
        match self.target {
            ReservationTarget::EngineRuns { turn } => self
                .runs()
                .and_then(|runs| remove_turn(&runs, turn))
                .map_err(|e| {
                    Refused::new(format!("its run's reservation can't be taken back: {e}"))
                }),
            ReservationTarget::JobStream { ref path, from } => state_reads::append(path)
                .and_then(|file| file.set_len(from))
                .map_err(|e| {
                    Refused::new(format!("its run's reservation can't be taken back: {e}"))
                }),
        }
    }

    /// Record what the run's stream (`stdout`, all of it that was read) says
    /// of its session and cost: its `init` and `result` records, each whole
    /// line that parses. A run with no result stays held at its budget, as
    /// one stopped before its result is, until a later run of its session
    /// accounts for it. For a `JobStream` reservation, the host runner writes
    /// the streamed records directly to `N.jsonl`, so nothing is duplicated.
    pub fn finish(self, stdout: &[u8]) -> io::Result<()> {
        let ReservationTarget::EngineRuns { turn } = self.target else {
            return Ok(());
        };
        let mut lines = String::new();
        for line in stdout.split(|b| *b == b'\n') {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let kept = v["type"] == "result" || (v["type"] == "system" && v["subtype"] == "init");
            if kept {
                lines.push_str(&v.to_string());
                lines.push('\n');
            }
        }
        if lines.is_empty() {
            return Ok(());
        }
        self.runs()?
            .append_file(&format!("{turn}.jsonl"))?
            .write_all(lines.as_bytes())
    }
}

fn meter(credit: &Path, jobs: &Path, now: i64) -> Result<burrow_settings::credit::Meter, Refused> {
    burrow_probe::credit::meter(credit, Some(jobs), now)
        .map_err(|e| Refused::new(format!("its credit can't be metered: {e}")))
}

/// Whether the credit has room now, as an observer reads it (the drain's
/// look): no lock, nothing admitted.
pub fn room(credit_file: Option<&Path>, jobs: &Path, now: i64) -> bool {
    credit_file.is_some_and(|credit| meter(credit, jobs, now).is_ok_and(|m| m.room()))
}

/// Whether the credit is short at `pct` now, as a route reads it: no room
/// (none set, a meter that can't be read, the reserve or today's allowance
/// reached), or `pct` percent of its cycle's budget spent.
pub fn short(credit_file: Option<&Path>, jobs: &Path, pct: f64, now: i64) -> bool {
    let Some(credit) = credit_file else {
        return true;
    };
    meter(credit, jobs, now).map_or(true, |m| {
        !m.room() || m.spent as f64 * 100.0 >= pct * m.cycle.budget as f64
    })
}

/// When the clock alone may give the credit room again, as an observer
/// reads it ([`burrow_settings::credit::Meter::opens`]): what the drain books
/// a pass for when it has none. None with no credit set or a meter that
/// can't be read (fixing either is an event of its own), or while only a
/// run going holds it (its turn's end kicks the drain).
pub fn opens(credit_file: Option<&Path>, jobs: &Path, now: i64) -> Option<i64> {
    let credit = credit_file?;
    meter(credit, jobs, now).ok()?.opens()
}

/// Why a finished run was refused, when the API refused it (its key, its
/// credit or its budget): `last` is its result, `records` its stream. Any
/// other error is the turn's own, ended like any other.
pub fn refusal(last: &Value, records: impl Iterator<Item = Value>) -> Option<Refused> {
    if !stream::is_error(last) {
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
    let records = stream::lines(path, from)
        .ok()?
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok());
    refusal(last, records)
}

#[cfg(test)]
mod tests;
