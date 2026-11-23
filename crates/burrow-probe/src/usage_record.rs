//! Engine-agnostic normalized per-turn and per-run usage records (`<N>.usage.json`).
//!
//! Each completed turn writes one normalized JSON record beside its turn files so
//! `burrow-probe` and `burrow-dashboard` aggregate token and cost totals across any
//! engine without parsing provider-specific stream logs.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;

use burrow_process::state_reads::Dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answer::usage::accounting::{Invocation, Report, Session, decode};

/// Filename suffix for a turn's normalized usage record (`<N>.usage.json`).
pub const SUFFIX: &str = ".usage.json";
/// Current schema version for [`TurnUsage`].
pub const VERSION: u32 = 1;

/// Normalized usage and cost metadata for one turn (`<N>.usage.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnUsage {
    pub version: u32,
    /// Primary or final engine name that ran this turn.
    pub engine: String,
    /// Optional model identifier used for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// One entry per invocation/run within this turn, in execution order.
    #[serde(default)]
    pub runs: Vec<RunUsage>,
}

/// Normalized token and cost accounting for one engine invocation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunUsage {
    /// Engine name that executed this run.
    pub engine: String,
    /// Unix timestamp (seconds) when this run started.
    pub started_at: i64,
    /// Optional model identifier for this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional session identifier (used for cross-turn cumulative cost recovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Optional budget cap in micro-dollars (for budgeted runs such as API credit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_micros: Option<i64>,
    /// Prompt / input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Completion / output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cached prompt tokens read.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Cached prompt tokens written.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Incremental USD cost for this run, or `None` when unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Optional session-cumulative USD cost reported by providers whose meter is cumulative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cumulative_cost_usd: Option<f64>,
    /// True if token counts are partial (e.g. run crashed before final usage).
    #[serde(default)]
    pub tokens_partial: bool,
    /// True if cost is missing or incomplete for this run.
    #[serde(default)]
    pub cost_partial: bool,
    /// True if this run's cost may include earlier unreported runs in the same session.
    #[serde(default)]
    pub prior_gap: bool,
    /// Missing-cost run identifiers emitted by this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// Earlier missing-cost run identifiers recovered by this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

impl TurnUsage {
    /// Filename for turn `n`'s normalized usage record (`<n>.usage.json`).
    pub fn file_name(n: u64) -> String {
        format!("{n}{SUFFIX}")
    }

    /// Parse a normalized usage record from JSON bytes, validating its version.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let record: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if record.version != VERSION {
            return Err(format!(
                "unsupported usage record version {} (expected {VERSION})",
                record.version
            ));
        }
        if record.engine.trim().is_empty() {
            return Err("usage record engine cannot be empty".into());
        }
        if record.runs.iter().any(|r| r.engine.trim().is_empty()) {
            return Err("usage record run engine cannot be empty".into());
        }
        Ok(record)
    }

    /// Read turn `n`'s normalized usage record from `dir`, returning `Ok(None)` if absent.
    pub fn load(dir: &Dir, n: u64) -> Result<Option<Self>, String> {
        let name = Self::file_name(n);
        match dir.text(&name) {
            Ok(text) => Self::parse(text.as_bytes())
                .map(Some)
                .map_err(|e| format!("{name}: {e}")),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{name}: {e}")),
        }
    }

    /// Convert this engine-agnostic record into an accounting [`Report`] while updating
    /// cross-turn session state for any subsequent turns in the same job.
    pub fn to_report(&self, sessions: &mut BTreeMap<String, Session>, fallback: &str) -> Report {
        let mut report = Report {
            identity: fallback.to_owned(),
            ..Report::default()
        };
        for run in &self.runs {
            let tokens = [
                run.input_tokens,
                run.output_tokens,
                run.cache_read_tokens,
                run.cache_write_tokens,
            ];
            for (sum, val) in report.tokens.iter_mut().zip(tokens) {
                *sum += val;
            }
            report.tokens_partial |= run.tokens_partial;
            let cost_missing = run.cost_partial || run.cost_usd.is_none();
            report.cost_partial |= cost_missing;
            report.prior_gap |= run.prior_gap;
            let cost = run.cost_usd.unwrap_or(0.0);
            report.cost += cost;

            let mut inv_missing = std::collections::BTreeSet::new();
            if !run.missing.is_empty() {
                inv_missing.extend(run.missing.iter().cloned());
            } else if cost_missing {
                let id = format!("{fallback}:{}", report.missing.len());
                inv_missing.insert(id);
            }
            let inv_recovered: std::collections::BTreeSet<String> =
                run.recovered.iter().cloned().collect();

            report.missing.extend(inv_missing.iter().cloned());
            report.recovered.extend(inv_recovered.iter().cloned());

            if let Some(session_key) = run
                .session
                .as_deref()
                .or_else(|| run.cumulative_cost_usd.map(|_| fallback))
            {
                let entry = sessions.entry(session_key.to_owned()).or_default();
                if let Some(cum) = run.cumulative_cost_usd {
                    entry.cost = entry.cost.max(cum);
                }
                for id in &inv_recovered {
                    entry.gaps.remove(id);
                }
                for id in &inv_missing {
                    entry.gaps.insert(id.clone());
                }
            }

            report.invocations.push(Invocation {
                marked: true,
                at: Some(run.started_at),
                budget: run.budget_micros,
                engine: Some(run.engine.clone()),
                session: run.session.clone(),
                tokens,
                tokens_partial: run.tokens_partial,
                cost_partial: cost_missing,
                prior_gap: run.prior_gap,
                cost,
                cumulative_cost: run.cumulative_cost_usd,
                missing: inv_missing,
                recovered: inv_recovered,
            });
        }
        report
    }

    /// Build a normalized [`TurnUsage`] for a completed turn in `job_dir` from its
    /// stream and prior turns' session state.
    pub fn from_job_turn(
        job_dir: &Path,
        job_id: &str,
        turn: u64,
        engine: &str,
        model: Option<String>,
        fallback_started: i64,
    ) -> Result<Self, String> {
        let dir = Dir::open(job_dir).map_err(|e| format!("{}: {e}", job_dir.display()))?;
        let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
        let mut prior: Vec<u64> = dir
            .names()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|name| {
                let s = name.to_str()?;
                s.strip_suffix(SUFFIX)
                    .or_else(|| s.strip_suffix(".jsonl"))?
                    .parse::<u64>()
                    .ok()
            })
            .filter(|&m| m < turn)
            .collect();
        prior.sort_unstable();
        prior.dedup();
        for m in prior {
            let fallback = format!("{job_id}/{m}");
            if let Ok(Some(saved)) = Self::load(&dir, m) {
                saved.to_report(&mut sessions, &fallback);
            } else {
                let prev_engine = dir
                    .text(&format!("{m}.engine"))
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let prev_engine = if prev_engine.is_empty() {
                    "claude"
                } else {
                    prev_engine.as_str()
                };
                if crate::credit::claude_stream(prev_engine) {
                    let recs = jsonl_records(&dir, m);
                    decode(recs.into_iter(), &mut sessions, &fallback);
                }
            }
        }
        let started = dir
            .text(&format!("{turn}.started"))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(fallback_started);
        let recs = jsonl_records(&dir, turn);
        let report = decode(recs.into_iter(), &mut sessions, &format!("{job_id}/{turn}"));
        let runs = report
            .invocations
            .into_iter()
            .map(|inv| {
                let run_engine = inv.engine.unwrap_or_else(|| engine.to_owned());
                let cost_usd = (!inv.cost_partial || inv.cost > 0.0).then_some(inv.cost);
                RunUsage {
                    engine: run_engine,
                    started_at: inv.at.unwrap_or(started),
                    model: model.clone(),
                    session: inv.session,
                    budget_micros: inv.budget,
                    input_tokens: inv.tokens[0],
                    output_tokens: inv.tokens[1],
                    cache_read_tokens: inv.tokens[2],
                    cache_write_tokens: inv.tokens[3],
                    cost_usd,
                    cumulative_cost_usd: inv.cumulative_cost,
                    tokens_partial: inv.tokens_partial,
                    cost_partial: inv.cost_partial,
                    prior_gap: inv.prior_gap,
                    missing: inv.missing.into_iter().collect(),
                    recovered: inv.recovered.into_iter().collect(),
                }
            })
            .collect();
        Ok(Self {
            version: VERSION,
            engine: engine.to_owned(),
            model,
            runs,
        })
    }
}

fn jsonl_records(dir: &Dir, n: u64) -> Vec<Value> {
    let Ok(file) = dir.file(Path::new(&format!("{n}.jsonl"))) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}
