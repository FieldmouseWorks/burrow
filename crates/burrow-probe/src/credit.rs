//! The API credit meter as the machine stands: burrow-settings' credit
//! setting and meter, over the cycle's [`ENGINE`] run costs that proj
//! recorded in its jobs, each run of a turn at its own start, decoded by the same accounting the usage answer
//! uses (answer/usage/accounting.rs). A run is the credit's when its marker
//! names [`ENGINE`] (an older marker names none: its run was on the engine
//! its turn was on before its route first moved it onto the credit, which
//! proj keeps in N.[`credit::ENGINE_BEFORE`], or else its turn's engine), so
//! a turn its route moved between the subscription and the credit is
//! charged for its credit runs alone; proj keeps such a turn's engine
//! [`ENGINE`] once it has been on the credit, which is how its job is found. The probe's `usage` answer shows it,
//! and proj's admission of every [`ENGINE`] run reads the same [`meter`]
//! (burrow-engine-anthropic's api.rs).
//!
//! Strict where the usage answer is tolerant: anything about a job with an
//! [`ENGINE`] turn that can't be read (its folder, a turn's engine, start
//! or stream, a malformed record) is an error, and an error is no room. An
//! [`ENGINE`] run of the cycle whose cost isn't recorded (one still
//! running, or one stopped before its result that no later run of its
//! session has accounted for) is counted at the budget its marker gave it
//! (`--max-budget-usd`, the most it can spend, which proj's admission sets
//! from the room left and writes under the admission lock), so runs going
//! at once never share the same room. One whose marker gives no budget (a
//! run from before admission wrote one) makes the meter pending instead:
//! its spend can't be known, so there's no room until it is. The meter
//! still says what it knows meanwhile. A turn whose stream holds no record
//! hasn't started a run (proj writes the marker first), so it's nothing.

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

/// Every [`ENGINE`] run's cost that started at `from` or after: a turn's
/// runs (its first and each retry) each at the time its marker says.
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
    // Each turn's engine, and the one its runs from before markers named
    // theirs ran on: the engine it was on before it moved onto the credit,
    // if it did.
    let mut older = BTreeMap::new();
    for n in &turns {
        let engine = text(dir, &format!("{n}.engine"))?.unwrap_or_else(|| "claude".into());
        let before = text(dir, &format!("{n}.{}", credit::ENGINE_BEFORE))?;
        older.insert(*n, before.as_deref().unwrap_or(&engine) == ENGINE);
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
        let records = records(dir, n)?;
        // A turn waiting to start its first run: the one launching now, as
        // its admission reads the meter.
        if records.is_empty() {
            continue;
        }
        let report = decode(records.into_iter(), &mut sessions, &format!("{label}/{n}"));
        decoded.push((n, older[&n], report));
    }
    let recovered: BTreeSet<&String> = decoded.iter().flat_map(|(_, _, r)| &r.recovered).collect();
    let mut gaps = BTreeSet::new();
    for (n, api, report) in &decoded {
        // Each run of a turn counts at its own start: a retry keeps the
        // turn's first start in N.started and writes its own in its marker.
        let mut started = None;
        for run in &report.invocations {
            // A run on the engine its marker names; an older marker's, on
            // the engine its turn was on before it moved.
            let api = &run.engine.as_deref().map_or(*api, |e| e == ENGINE);
            let charged = *api || run.recovered.iter().any(|id| gaps.contains(id));
            if !charged {
                continue;
            }
            let at = if run.marked {
                run.at
                    .ok_or_else(|| format!("turn {n} has a run marker without its time"))?
            } else {
                match started {
                    Some(at) => at,
                    None => *started.insert(start(dir, *n)?),
                }
            };
            if at < from {
                continue;
            }
            let mut micros = micros(run.cost)?;
            if *api {
                if let Some(id) = run.missing.iter().find(|id| !recovered.contains(id)) {
                    match run.budget {
                        // The most it can spend, held until its cost is known.
                        Some(budget) if budget >= 0 => micros = micros.max(budget),
                        _ if ledger.pending.is_none() => {
                            ledger.pending = Some(format!(
                                "{ENGINE} turn {id} has no recorded cost yet: running, or stopped before its result"
                            ));
                        }
                        _ => {}
                    }
                }
                gaps.extend(run.missing.iter().cloned());
            }
            // A Claude run that accounts for an API run's unrecorded cost is
            // charged whole: its cost includes that run's.
            ledger.spends.push(Spend { at, micros });
        }
    }
    Ok(())
}

/// When turn N first started, for the runs of a stream from before markers.
fn start(dir: &Dir, n: u64) -> Result<i64, String> {
    text(dir, &format!("{n}.started"))?
        .ok_or_else(|| format!("turn {n} has no recorded start"))?
        .parse::<i64>()
        .map_err(|e| format!("{n}.started: {e}"))
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
