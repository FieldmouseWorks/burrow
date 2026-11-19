//! The API credit meter as the machine stands: burrow-settings' credit
//! setting and meter, over the cycle's [`ENGINE`] turn costs that proj
//! recorded in its jobs, decoded by the same accounting the usage answer
//! uses (answer/usage/accounting.rs). The probe's `usage` answer shows it,
//! and proj's admission and routing read the same [`meter`].
//!
//! Strict where the usage answer is tolerant: anything about a job with an
//! [`ENGINE`] turn that can't be read (its folder, a turn's engine, start
//! or stream, a malformed record) is an error, and an error is no room. An
//! [`ENGINE`] turn of the cycle whose cost isn't recorded (one still
//! running, or one stopped before its result that no later run of its
//! session has accounted for) is counted at what it has recorded and makes
//! the meter pending: its spend can't be known, so there's no room until it
//! is, and [`ENGINE`] turns therefore run one at a time. The meter still
//! says what it knows meanwhile.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{ErrorKind, Read};
use std::path::Path;

use burrow_process::state_reads::{self, Dir};
pub use burrow_settings::credit::ENGINE;
use burrow_settings::credit::{self, Credit, Meter, Spend};
use serde_json::Value;

use crate::answer::usage::accounting::{Report, Session, decode};

/// The engines whose turns stream Claude Code's records, and so share a
/// job's sessions and their cumulative cost.
pub fn claude_stream(engine: &str) -> bool {
    engine == "claude" || engine == ENGINE
}

/// The meter at `now`, from the setting in `credit_file` and the turns
/// under `jobs`. Any error is no room.
pub fn meter(credit_file: &Path, jobs: Option<&Path>, now: i64) -> Result<Meter, String> {
    let credit = setting(credit_file)?;
    let cycle = credit.cycle(now)?;
    let jobs = jobs.ok_or("proj's jobs aren't configured here")?;
    let ledger = ledger(jobs, cycle.start)?;
    let mut meter = Meter::at(&credit, now, &ledger.spends)?;
    meter.pending = ledger.pending;
    Ok(meter)
}

/// The cycle's [`ENGINE`] spend as recorded, and the first turn whose cost
/// isn't, if any.
#[derive(Debug, Default)]
pub struct Ledger {
    pub spends: Vec<Spend>,
    pub pending: Option<String>,
}

fn setting(path: &Path) -> Result<Credit, String> {
    let bytes = match state_reads::bytes(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err("no API credit is set".into()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    credit::parse(&bytes)?.ok_or_else(|| "no API credit is set".into())
}

/// Every [`ENGINE`] turn's cost that started at `from` or after.
pub fn ledger(jobs: &Path, from: i64) -> Result<Ledger, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", jobs.display());
    let root = Dir::open(jobs).map_err(fail)?;
    let mut ledger = Ledger::default();
    for name in root.names().map_err(fail)? {
        let label = name.to_string_lossy().into_owned();
        // Hidden entries are proj's control state, not jobs.
        if label.starts_with('.') {
            continue;
        }
        let dir = match root.dir(Path::new(&name)) {
            Ok(dir) => dir,
            // A file beside the jobs, or a job removed since the listing.
            Err(e) if matches!(e.kind(), ErrorKind::NotADirectory | ErrorKind::NotFound) => {
                continue;
            }
            Err(e) => return Err(format!("job {label}: {e}")),
        };
        job(&dir, &label, from, &mut ledger).map_err(|e| format!("job {label}: {e}"))?;
    }
    Ok(ledger)
}

/// A turn file's text: None when there's none.
fn text(dir: &Dir, name: &str) -> Result<Option<String>, String> {
    match dir.text(name) {
        Ok(text) => Ok(Some(text.trim().to_owned())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{name}: {e}")),
    }
}

/// A turn's stream, every whole line a record. A last line still being
/// written (no newline yet) is left for the next read.
fn records(dir: &Dir, n: u64) -> Result<Vec<Value>, String> {
    let name = format!("{n}.jsonl");
    let mut bytes = Vec::new();
    dir.file(Path::new(&name))
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|e| format!("{name}: {e}"))?;
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    lines.pop(); // what follows the last newline: empty, or still being written
    lines
        .into_iter()
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .map(|line| serde_json::from_slice(line).map_err(|e| format!("{name}: {e}")))
        .collect()
}

fn job(dir: &Dir, label: &str, from: i64, ledger: &mut Ledger) -> Result<(), String> {
    let mut turns: Vec<u64> = dir
        .names()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|name| name.to_str()?.strip_suffix(".jsonl")?.parse().ok())
        .collect();
    turns.sort_unstable();
    let mut engines = BTreeMap::new();
    for n in &turns {
        let engine = text(dir, &format!("{n}.engine"))?.unwrap_or_else(|| "claude".into());
        engines.insert(*n, engine);
    }
    if !engines.values().any(|e| e == ENGINE) {
        return Ok(());
    }
    // Every Claude Code turn in order, through one set of sessions: cost is
    // cumulative per session, and a session can move between engines.
    let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
    let mut decoded: Vec<(u64, bool, Report)> = Vec::new();
    for n in turns {
        let engine = &engines[&n];
        if !claude_stream(engine) {
            continue;
        }
        let report = decode(
            records(dir, n)?.into_iter(),
            &mut sessions,
            &format!("{label}/{n}"),
        );
        decoded.push((n, engine == ENGINE, report));
    }
    let recovered: BTreeSet<&String> = decoded.iter().flat_map(|(_, _, r)| &r.recovered).collect();
    let mut gaps = BTreeSet::new();
    for (n, api, report) in &decoded {
        let charged = *api || report.recovered.iter().any(|id| gaps.contains(id));
        if !charged {
            continue;
        }
        let at = text(dir, &format!("{n}.started"))?
            .ok_or_else(|| format!("turn {n} has no recorded start"))?
            .parse::<i64>()
            .map_err(|e| format!("{n}.started: {e}"))?;
        if at < from {
            continue;
        }
        if *api {
            if let Some(id) = report.missing.iter().find(|id| !recovered.contains(id))
                && ledger.pending.is_none()
            {
                ledger.pending = Some(format!(
                    "{ENGINE} turn {id} has no recorded cost yet: running, or stopped before its result"
                ));
            }
            gaps.extend(report.missing.iter().cloned());
        }
        // A Claude turn that accounts for an API turn's unrecorded run is
        // charged whole: its cost includes that run's.
        ledger.spends.push(Spend {
            at,
            micros: micros(report.cost)?,
        });
    }
    Ok(())
}

fn micros(usd: f64) -> Result<i64, String> {
    let m = (usd * 1e6).round();
    if !m.is_finite() || m < 0.0 || m > i64::MAX as f64 / 4.0 {
        return Err(format!("a turn's cost of {usd} can't be counted"));
    }
    Ok(m as i64)
}

#[cfg(test)]
mod tests;
