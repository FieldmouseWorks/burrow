//! Structured usage only: prompts and result text never enter the answer.
use super::{Machine, utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
fn read(p: &Path, name: &str) -> String {
    fs::read_to_string(p.join(name))
        .unwrap_or_default()
        .trim()
        .into()
}
fn records(p: &Path) -> impl Iterator<Item = Value> {
    File::open(p)
        .ok()
        .into_iter()
        .flat_map(|f| BufReader::new(f).lines().map_while(Result::ok))
        .filter_map(|l| serde_json::from_str(&l).ok())
}
fn time(p: &Path) -> Option<i64> {
    Some(
        p.metadata()
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64,
    )
}
fn turn_time(dir: &Path, n: u64) -> Option<i64> {
    time(&dir.join(format!("{n}.slot"))).or_else(|| time(&dir.join(format!("{n}.jsonl"))))
}
const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];
#[derive(Default)]
struct Spent {
    tokens: [u64; 4],
    cost: f64,
}
impl Spent {
    fn result(v: &Value) -> Self {
        Self {
            tokens: FIELDS.map(|k| v["usage"][k].as_u64().unwrap_or(0)),
            cost: v["total_cost_usd"].as_f64().unwrap_or(0.0),
        }
    }
    fn delta(&self, old: &Self) -> Self {
        Self {
            tokens: std::array::from_fn(|i| self.tokens[i].saturating_sub(old.tokens[i])),
            cost: (self.cost - old.cost).max(0.0),
        }
    }
    fn add(&mut self, other: &Self) {
        for (sum, value) in self.tokens.iter_mut().zip(other.tokens) {
            *sum += value;
        }
        self.cost += other.cost;
    }
    fn show(&self) -> String {
        format!(
            "input={} output={} cache-read={} cache-write={}",
            self.tokens[0], self.tokens[1], self.tokens[2], self.tokens[3]
        )
    }
}
pub(super) fn answer(m: &Machine, since: i64) -> Result<String, String> {
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(jobs) = &m.cfg.jobs {
        let root = m.path(&jobs.to_string_lossy());
        for entry in fs::read_dir(&root).map_err(|e| format!("{}: {e}", root.display()))? {
            let dir = entry.map_err(|e| e.to_string())?.path();
            if !dir.is_dir() {
                continue;
            }
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
            let mut turns: Vec<u64> = fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".jsonl")?.parse().ok())
                .collect();
            turns.sort_unstable();
            // Historical jobs need no stream reads. For a selected job, earlier
            // turns establish the session totals before the requested span.
            if !turns
                .iter()
                .any(|n| turn_time(&dir, *n).is_some_and(|at| at >= since && at <= m.now))
            {
                continue;
            }
            let mut sessions: BTreeMap<String, Spent> = BTreeMap::new();
            for n in turns {
                let stream = dir.join(format!("{n}.jsonl"));
                let at = turn_time(&dir, n).unwrap_or(0);
                let engine = read(&dir, &format!("{n}.engine"));
                let engine = if engine.is_empty() { "claude" } else { &engine };
                let mut session = format!("turn-{n}");
                let mut spent = Spent::default();
                let mut has_result = false;
                let mut messages = BTreeMap::new();
                if engine == "claude" {
                    for v in records(&stream) {
                        if let Some(id) = v["session_id"].as_str() {
                            session = id.into();
                        }
                        if v["type"] == "result"
                            && v["usage"].is_object()
                            && v["total_cost_usd"].is_number()
                        {
                            let result = Spent::result(&v);
                            spent.add(&result.delta(sessions.entry(session.clone()).or_default()));
                            sessions.insert(session.clone(), result);
                            has_result = true;
                        }
                        if v["type"] == "assistant"
                            && let Some(id) = v["message"]["id"].as_str()
                        {
                            messages.insert(
                                id.to_owned(),
                                FIELDS.map(|k| v["message"]["usage"][k].as_u64().unwrap_or(0)),
                            );
                        }
                    }
                }
                if at < since || at > m.now {
                    continue;
                }
                *counts.entry(engine.into()).or_default() += 1;
                let detail = if engine != "claude" {
                    String::new()
                } else if has_result {
                    format!("{} cost=${:.6}", spent.show(), spent.cost)
                } else {
                    let mut s = Spent::default();
                    for t in messages.values() {
                        for (sum, v) in s.tokens.iter_mut().zip(t) {
                            *sum += v;
                        }
                    }
                    format!("{}; no result: partial tokens, cost unavailable", s.show())
                };
                groups
                    .entry((engine.into(), kind.clone()))
                    .or_default()
                    .push(format!(
                        "  {}{reference} turn {n} {} {detail}\n",
                        dir.file_name().unwrap_or_default().to_string_lossy(),
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
    for ((engine, kind), turns) in groups {
        out += &format!("{engine} / {kind}: {} turns\n", turns.len());
        for turn in turns {
            out += &turn;
        }
    }
    out += "Meter readings:\n";
    let log = m.path(
        &m.cfg
            .state_dir
            .join("usage-reset/meters/log")
            .to_string_lossy(),
    );
    if !log.exists() {
        out += "unavailable (no meter log)\n";
    }
    for v in records(&log) {
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
    Ok(out)
}
