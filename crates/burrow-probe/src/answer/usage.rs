//! Structured usage only: prompts and result text never enter the answer.
use super::{Machine, utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
mod accounting;
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
/// `totals` keeps the table alone; otherwise meter readings and each turn
/// follow it. Comment fitting preserves the table and budgets optional detail.
pub(super) fn answer(m: &Machine, since: i64, only_totals: bool) -> Result<String, String> {
    let mut table = Totals::default();
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    let mut unstarted: BTreeMap<String, usize> = BTreeMap::new();
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
            let link = read(&dir, "link");
            let reference = link
                .strip_prefix("https://github.com/")
                .and_then(|s| s.split_once("/issues/"))
                .filter(|(repo, n)| {
                    crate::query::repository(repo) && n.bytes().all(|b| b.is_ascii_digit())
                })
                .map(|(repo, n)| format!(" {repo}#{n}"))
                .unwrap_or_default();
            let role = read(&dir, "role");
            let mut inbox = read(&dir, "usage-kind");
            if inbox.is_empty() && !reference.is_empty() {
                inbox = "inbox labels=unknown opener=unknown (not recorded)".into();
            }
            let role_key = match (role.is_empty(), inbox.is_empty()) {
                (false, _) => role.clone(),
                (true, false) => "(inbox)".into(),
                (true, true) => "(none)".into(),
            };
            let project = project(&dir);
            let mut kind = if role.is_empty() {
                String::new()
            } else {
                format!("role {role}; ")
            };
            if !inbox.is_empty() {
                kind += &inbox;
            } else if role.is_empty() {
                let starter = read(&dir, "starter");
                kind += &format!(
                    "no role; starter={}",
                    if !starter.is_empty() {
                        starter
                    } else {
                        "unknown (not recorded)".into()
                    }
                );
            }
            let mut turns: Vec<u64> = dir
                .names()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter_map(|name| name.to_str()?.strip_suffix(".jsonl")?.parse().ok())
                .collect();
            turns.sort_unstable();
            for n in &turns {
                if turn_time(&dir, *n).is_none() {
                    let engine = read(&dir, &format!("{n}.engine"));
                    let empty = dir
                        .file(Path::new(&format!("{n}.jsonl")))
                        .ok()
                        .and_then(|f| f.metadata().ok())
                        .is_some_and(|m| m.len() == 0);
                    let counts = if empty { &mut unstarted } else { &mut unknown };
                    *counts
                        .entry(if engine.is_empty() {
                            "claude".into()
                        } else {
                            engine
                        })
                        .or_default() += 1;
                }
            }
            if !turns
                .iter()
                .any(|n| turn_time(&dir, *n).is_some_and(|at| at >= since && at <= m.now))
            {
                continue;
            }
            let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
            for n in turns {
                let at = turn_time(&dir, n);
                let engine = read(&dir, &format!("{n}.engine"));
                let engine = if engine.is_empty() { "claude" } else { &engine };
                let report = if engine == "claude" {
                    Some(decode(
                        records(dir.file(Path::new(&format!("{n}.jsonl"))).ok()),
                        &mut sessions,
                        &format!("{}/{n}", name.to_string_lossy()),
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
                *counts.entry(engine.into()).or_default() += 1;
                table.add(engine, &role_key, &project, report.as_ref());
                let detail = report.map(|r| r.show()).unwrap_or_default();
                groups
                    .entry((engine.into(), kind.clone()))
                    .or_default()
                    .push(format!(
                        "  {}{reference} turn {n} {} {detail}\n",
                        name.to_string_lossy(),
                        utc(at)
                    ));
            }
        }
    }
    let mut out = format!(
        "Usage {} to {} (turn start time; stream cost is not plan utilization):\n",
        utc(since),
        utc(m.now)
    );
    for engine in ["claude", "codex"] {
        out += &format!("{engine}: {} turns\n", counts.get(engine).unwrap_or(&0));
    }
    for (engine, n) in unknown {
        out += &format!(
            "{engine}: {n} historical turns with unknown first start (excluded from span totals)\n"
        );
    }
    for (engine, n) in unstarted {
        out += &format!(
            "{engine}: {n} turns without recorded execution (empty stream; excluded from span totals)\n"
        );
    }
    out += &table.show();
    if only_totals {
        return Ok(out);
    }
    out += "Meter readings:\n";
    let log = m.path(
        &m.cfg
            .state_dir
            .join("usage-reset/meters/log")
            .to_string_lossy(),
    );
    let log = burrow_process::state_reads::open(&log).ok();
    if log.is_none() {
        out += "unavailable (no meter log)\n";
    }
    for v in records(log) {
        let Some(at) = v["read_at"].as_f64() else {
            continue;
        };
        if at < since as f64 || at > m.now as f64 {
            continue;
        }
        let Some(engine @ ("claude" | "codex")) = v["engine"].as_str() else {
            continue;
        };
        if let Some(ws) = v["windows"].as_object() {
            for (name, w) in ws {
                let name = match name.as_str() {
                    "five_hour" | "300m" => "five_hour",
                    "seven_day" | "10080m" => "seven_day",
                    _ => continue,
                };
                if let Some(used) = w["used"].as_f64() {
                    out += &format!(
                        "{} {engine} {name}: {used}%{}\n",
                        utc(at as i64),
                        if used >= 100.0 { " HIT 100%" } else { "" }
                    );
                }
            }
        }
    }
    out += "Each turn:\n";
    for ((engine, kind), turns) in groups {
        out += &format!("{engine} / {kind}: {} turns\n", turns.len());
        for turn in turns {
            out += &turn;
        }
    }
    Ok(out)
}
