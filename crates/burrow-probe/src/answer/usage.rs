//! Structured usage only: prompts and result text never enter the answer.
use super::{Machine, utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
pub(crate) mod accounting;
mod totals;
use accounting::{Session, decode};
use burrow_process::state_reads::Dir;
use totals::Totals;
fn read(dir: &Dir, name: &str) -> String {
    dir.text(name).unwrap_or_default().trim().into()
}
fn records(file: Option<File>) -> impl Iterator<Item = Value> {
    file.into_iter()
        .flat_map(|f| BufReader::new(f).lines().map_while(Result::ok))
        .filter_map(|l| serde_json::from_str(&l).ok())
}
fn turn_time(dir: &Dir, n: u64) -> Option<i64> {
    read(dir, &format!("{n}.started")).parse().ok()
}
/// The job's project: its checkout's folder name, which is its repo's.
fn project(dir: &Dir) -> String {
    let path = read(dir, "project");
    Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)")
        .into()
}
/// The API credit as it stands now, whatever the span: the meter proj's
/// admission reads, with no credential in it.
fn credit(m: &Machine) -> String {
    let file = m.path(
        &m.cfg
            .state_dir
            .join(burrow_settings::credit::FILE)
            .to_string_lossy(),
    );
    let jobs = m.cfg.jobs.as_ref().map(|j| m.path(&j.to_string_lossy()));
    let meter = match crate::credit::meter(&file, jobs.as_deref(), m.now) {
        Ok(meter) => meter.text(),
        Err(e) => format!("{} credit: no room ({e})\n", crate::credit::ENGINE),
    };
    format!("API credit at {}:\n{meter}", utc(m.now))
}

#[derive(Default)]
struct UsageScan {
    table: Totals,
    groups: BTreeMap<(String, String), Vec<String>>,
    counts: BTreeMap<String, usize>,
    unknown: BTreeMap<String, usize>,
    unstarted: BTreeMap<String, usize>,
}

fn record_undated_turns(dir: &Dir, turns: &[u64], scan: &mut UsageScan) {
    for n in turns {
        if turn_time(dir, *n).is_none() {
            let normalized = crate::usage_record::TurnUsage::load(dir, *n).ok().flatten();
            let engine = read(dir, &format!("{n}.engine"));
            let engine = if !engine.is_empty() {
                engine
            } else if let Some(u) = &normalized {
                u.engine.clone()
            } else {
                "claude".into()
            };
            let empty = normalized.is_none()
                && dir
                    .file(Path::new(&format!("{n}.jsonl")))
                    .ok()
                    .and_then(|f| f.metadata().ok())
                    .is_some_and(|m| m.len() == 0);
            let counts = if empty {
                &mut scan.unstarted
            } else {
                &mut scan.unknown
            };
            *counts.entry(engine).or_default() += 1;
        }
    }
}

fn job_metadata(dir: &Dir) -> (String, String, String, String) {
    let link = read(dir, "link");
    let reference = link
        .strip_prefix("https://github.com/")
        .and_then(|s| s.split_once("/issues/"))
        .filter(|(repo, n)| crate::query::repository(repo) && n.bytes().all(|b| b.is_ascii_digit()))
        .map(|(repo, n)| format!(" {repo}#{n}"))
        .unwrap_or_default();
    let role = read(dir, "role");
    let mut inbox = read(dir, "usage-kind");
    if inbox.is_empty() && !reference.is_empty() {
        inbox = "inbox labels=unknown opener=unknown (not recorded)".into();
    }
    let role_key = match (role.is_empty(), inbox.is_empty()) {
        (false, _) => role.clone(),
        (true, false) => "(inbox)".into(),
        (true, true) => "(none)".into(),
    };
    let project = project(dir);
    let mut kind = if role.is_empty() {
        String::new()
    } else {
        format!("role {role}; ")
    };
    if !inbox.is_empty() {
        kind += &inbox;
    } else if role.is_empty() {
        let starter = read(dir, "starter");
        kind += &format!(
            "no role; starter={}",
            if !starter.is_empty() {
                starter
            } else {
                "unknown (not recorded)".into()
            }
        );
    }
    (reference, role_key, project, kind)
}

fn scan_job_dir(
    m: &Machine,
    since: i64,
    name: &std::ffi::OsStr,
    dir: &Dir,
    scan: &mut UsageScan,
) -> Result<(), String> {
    let (reference, role_key, project, kind) = job_metadata(dir);
    let mut turns: Vec<u64> = dir
        .names()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|name| {
            let s = name.to_str()?;
            s.strip_suffix(crate::usage_record::SUFFIX)
                .or_else(|| s.strip_suffix(".jsonl"))?
                .parse()
                .ok()
        })
        .collect();
    turns.sort_unstable();
    turns.dedup();
    record_undated_turns(dir, &turns, scan);
    if !turns
        .iter()
        .any(|n| turn_time(dir, *n).is_some_and(|at| at >= since && at <= m.now))
    {
        return Ok(());
    }
    let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
    for n in turns {
        let at = turn_time(dir, n);
        let normalized = crate::usage_record::TurnUsage::load(dir, n).ok().flatten();
        let engine_file = read(dir, &format!("{n}.engine"));
        let engine = if !engine_file.is_empty() {
            engine_file
        } else if let Some(u) = &normalized {
            u.engine.clone()
        } else {
            "claude".into()
        };
        let fallback = format!("{}/{n}", name.to_string_lossy());
        let report = if let Some(u) = normalized {
            Some(u.to_report(&mut sessions, &fallback))
        } else if crate::credit::claude_stream(&engine) {
            Some(decode(
                records(dir.file(Path::new(&format!("{n}.jsonl"))).ok()),
                &mut sessions,
                &fallback,
            ))
        } else {
            None
        };
        let Some(at) = at else {
            continue;
        };
        if at < since || at > m.now {
            continue;
        }
        *scan.counts.entry(engine.clone()).or_default() += 1;
        scan.table
            .add(&engine, &role_key, &project, report.as_ref());
        let detail = report.map(|r| r.show()).unwrap_or_default();
        scan.groups
            .entry((engine, kind.clone()))
            .or_default()
            .push(format!(
                "  {}{reference} turn {n} {} {detail}\n",
                name.to_string_lossy(),
                utc(at)
            ));
    }
    Ok(())
}

fn append_meter_readings(out: &mut String, m: &Machine, since: i64) {
    *out += "Meter readings:\n";
    let log = m.path(
        &m.cfg
            .state_dir
            .join("usage-reset/meters/log")
            .to_string_lossy(),
    );
    let log = burrow_process::state_reads::open(&log).ok();
    if log.is_none() {
        *out += "unavailable (no meter log)\n";
    }
    for v in records(log) {
        let Some(at) = v["read_at"].as_f64() else {
            continue;
        };
        if at < since as f64 || at > m.now as f64 {
            continue;
        }
        let Some(engine) = v["engine"].as_str().filter(|e| {
            !e.is_empty()
                && *e != "codex"
                && e.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) else {
            continue;
        };
        if let Some(ws) = v["windows"].as_object() {
            for (name, w) in ws {
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    continue;
                }
                if let Some(used) = w["used"].as_f64() {
                    *out += &format!(
                        "{} {engine} {name}: {used}%{}\n",
                        utc(at as i64),
                        if used >= 100.0 { " HIT 100%" } else { "" }
                    );
                }
            }
        }
    }
}

/// `totals` keeps the table and the API credit alone; otherwise meter readings and each turn
/// follow it. Comment fitting preserves the table and budgets optional detail.
pub(super) fn answer(m: &Machine, since: i64, only_totals: bool) -> Result<String, String> {
    let mut scan = UsageScan::default();
    if let Some(jobs) = &m.cfg.jobs {
        let root = m.path(&jobs.to_string_lossy());
        let root = Dir::open(&root).map_err(|e| e.to_string())?;
        for name in root.names().map_err(|e| e.to_string())? {
            // Hidden directories hold proj control state, not job turns.
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(dir) = root.dir(Path::new(&name)) else {
                continue;
            };
            scan_job_dir(m, since, &name, &dir, &mut scan)?;
        }
    }
    let mut out = format!(
        "Usage {} to {} (turn start time; stream cost is not plan utilization):\n",
        utc(since),
        utc(m.now)
    );
    // Claude's always, and any other engine the window's turns ran on.
    scan.counts.entry("claude".into()).or_default();
    for (engine, n) in &scan.counts {
        out += &format!("{engine}: {n} turns\n");
    }
    for (engine, n) in scan.unknown {
        out += &format!(
            "{engine}: {n} historical turns with unknown first start (excluded from span totals)\n"
        );
    }
    for (engine, n) in scan.unstarted {
        out += &format!(
            "{engine}: {n} turns without recorded execution (empty stream; excluded from span totals)\n"
        );
    }
    out += &scan.table.show();
    out += &credit(m);
    if only_totals {
        return Ok(out);
    }
    append_meter_readings(&mut out, m, since);
    out += "Each turn:\n";
    for ((engine, kind), turns) in scan.groups {
        out += &format!("{engine} / {kind}: {} turns\n", turns.len());
        for turn in turns {
            out += &turn;
        }
    }
    Ok(out)
}
