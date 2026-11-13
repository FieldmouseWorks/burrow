//! Structured usage only: prompts and result text never enter the answer.
use super::{Machine, utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
mod totals;
use burrow_accounting::{Job, Meta, Tail, Tally, describe_prices};
use burrow_process::state_reads::Dir;
use totals::Totals;
fn read(dir: &Dir, name: &str) -> String {
    dir.text(name).unwrap_or_default().trim().into()
}
/// Where the usage answer's optional detail starts: what a comment cuts
/// to fit, after the required totals.
pub(crate) const DETAIL: &str = "By job, the most money first (lifetime: every turn, unfinished ones to their last complete record; span: turns started in it):\n";
/// One job's lifetime and span totals: context is the latest turn's
/// session occupancy, apart from the token traffic summed over every call.
fn job_line(name: &str, life: &Tally, span: &Tally) -> String {
    let unfinished = if life.unfinished > 0 {
        format!(", {} unfinished", life.unfinished)
    } else {
        String::new()
    };
    let context = match (life.context, life.context_peak) {
        (Some(now), peak) => format!("context={now} peak={}", peak.unwrap_or(now)),
        (None, Some(peak)) => format!("context unavailable peak={peak}"),
        (None, None) => "context unavailable".into(),
    };
    format!(
        "  {name}: {} turns ({} in span{unfinished}), {} sessions; input={} output={} cache-read={} cache-write={} {}; span {}; {context}\n",
        life.turns,
        span.turns,
        life.sessions.len(),
        life.tokens[0],
        life.tokens[1],
        life.tokens[2],
        life.tokens[3],
        life.money(),
        span.money(),
    )
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
    let mut by_job: Vec<(f64, String)> = Vec::new();
    let prices = &m.cfg.prices;
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
            // Every turn of the job is replayed, in the span or not, so a
            // resumed one counts only what it added and the job's lifetime
            // reconciles with its turns.
            let mut ledger = Job::new(prices.clone());
            let mut span = Tally::default();
            let last = turns.last().copied();
            for n in turns {
                let at = turn_time(&dir, n);
                let engine = read(&dir, &format!("{n}.engine"));
                let engine = if engine.is_empty() { "claude" } else { &engine };
                let model = Some(read(&dir, &format!("{n}.model"))).filter(|m| !m.is_empty());
                // The latest turn with no outcome is under way, or stopped
                // without saying: a snapshot to its last complete record.
                let unfinished = Some(n) == last
                    && ["outcome", "ended"]
                        .iter()
                        .all(|f| dir.file(Path::new(&format!("{n}.{f}"))).is_err());
                ledger.begin(Meta {
                    identity: format!("{}/{n}", name.to_string_lossy()),
                    engine: engine.into(),
                    model,
                    started: at,
                });
                if let Ok(mut stream) = dir.file(Path::new(&format!("{n}.jsonl"))) {
                    for v in Tail::default()
                        .read(&mut stream, !unfinished)
                        .unwrap_or_default()
                    {
                        ledger.feed(&v);
                    }
                }
                if !unfinished {
                    ledger.end();
                }
                let Some(at) = at else {
                    continue;
                };
                if at < since || at > m.now {
                    continue;
                }
                let turn = ledger.last().expect("the turn just begun");
                *counts.entry(engine.into()).or_default() += 1;
                table.add(engine, &role_key, &project, &turn);
                span.add_turn(&turn);
                let detail = turn
                    .report
                    .map(|r| r.show(turn.estimate.as_ref()))
                    .unwrap_or_default();
                groups
                    .entry((engine.into(), kind.clone()))
                    .or_default()
                    .push(format!(
                        "  {}{reference} turn {n} {} {detail}\n",
                        name.to_string_lossy(),
                        utc(at)
                    ));
            }
            let life = ledger.totals();
            by_job.push((
                life.cost + life.estimate,
                job_line(
                    &format!("{}{reference}", name.to_string_lossy()),
                    &life,
                    &span,
                ),
            ));
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
    out += &format!("Prices: {}.\n", describe_prices(prices));
    out += &table.show();
    if only_totals {
        return Ok(out);
    }
    // A day can hold hundreds of jobs: they lead the optional detail, the
    // most money first, so a cut leaves out the cheapest.
    out += DETAIL;
    by_job.sort_by(|(a, x), (b, y)| b.total_cmp(a).then_with(|| x.cmp(y)));
    for (_, job) in by_job {
        out += &job;
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
