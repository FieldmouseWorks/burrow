//! Result usage is per invocation; only cost is cumulative across resumes.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];
#[derive(Default)]
pub struct Session {
    pub(crate) cost: f64,
    pub(crate) gaps: BTreeSet<String>,
}
#[derive(Default)]
pub struct Report {
    pub tokens: [u64; 4],
    pub cost: f64,
    pub tokens_partial: bool,
    pub cost_partial: bool,
    pub prior_gap: bool,
    /// Run identities allow aggregates to match missing costs to later recovery.
    pub missing: BTreeSet<String>,
    pub recovered: BTreeSet<String>,
    /// The same, per host-marked invocation, in stream order.
    pub invocations: Vec<Invocation>,
    pub(crate) identity: String,
}
/// What a turn's stream added from one host `burrow.run` marker to the next.
/// Records before any marker (older streams have none) are one unmarked.
#[derive(Default)]
pub struct Invocation {
    pub marked: bool,
    /// The marker's time, when it says one.
    pub at: Option<i64>,
    /// The most the run may spend, in micro-dollars, when its marker says
    /// (proj gives a `claude-api` run `--max-budget-usd` and writes it here).
    pub budget: Option<i64>,
    /// The engine the run was on, when its marker says (an older one
    /// doesn't: its turn's engine is its own).
    pub engine: Option<String>,
    /// The model used for this invocation, when recorded by the host marker or stream.
    pub model: Option<String>,
    pub session: Option<String>,
    pub tokens: [u64; 4],
    pub tokens_partial: bool,
    pub cost_partial: bool,
    pub prior_gap: bool,
    pub cost: f64,
    pub cumulative_cost: Option<f64>,
    pub missing: BTreeSet<String>,
    pub recovered: BTreeSet<String>,
}
impl Report {
    fn invocation(&mut self) -> &mut Invocation {
        if self.invocations.is_empty() {
            self.invocations.push(Invocation::default());
        }
        self.invocations.last_mut().expect("one was just pushed")
    }
    fn add(&mut self, tokens: [u64; 4]) {
        for (sum, value) in self.tokens.iter_mut().zip(tokens) {
            *sum += value;
        }
    }
    pub fn show(&self) -> String {
        let cost_label = if self.cost_partial {
            "known-cost"
        } else {
            "cost"
        };
        let mut s = format!(
            "input={} output={} cache-read={} cache-write={} {cost_label}=${:.6}",
            self.tokens[0], self.tokens[1], self.tokens[2], self.tokens[3], self.cost
        );
        if self.tokens_partial {
            s += "; partial tokens (output counts completed runs only)";
        }
        if self.cost_partial {
            s += "; additional cost unavailable";
        }
        if self.prior_gap {
            s += "; cost may include unreported earlier runs";
        }
        s
    }
}
#[derive(Default)]
struct Run {
    active: bool,
    initialized: bool,
    messages: BTreeMap<String, [u64; 4]>,
}
fn partial(
    run: &mut Run,
    session: &str,
    sessions: &mut BTreeMap<String, Session>,
    report: &mut Report,
) {
    report.cost_partial = true;
    report.invocation().cost_partial = true;
    missing(session, sessions, report);
    partial_tokens(run, report);
    *run = Run::default();
}
fn missing(session: &str, sessions: &mut BTreeMap<String, Session>, report: &mut Report) {
    let id = format!("{}:{}", report.identity, report.missing.len());
    let has_session = session != report.identity;
    report.missing.insert(id.clone());
    let inv = report.invocation();
    inv.missing.insert(id.clone());
    if inv.session.is_none() && has_session {
        inv.session = Some(session.to_owned());
    }
    sessions.entry(session.into()).or_default().gaps.insert(id);
}
fn partial_tokens(run: &mut Run, report: &mut Report) {
    report.tokens_partial = true;
    report.invocation().tokens_partial = true;
    for mut tokens in std::mem::take(&mut run.messages).into_values() {
        // Assistant output usage is a placeholder until the result arrives.
        tokens[1] = 0;
        report.add(tokens);
        for (sum, value) in report.invocation().tokens.iter_mut().zip(tokens) {
            *sum += value;
        }
    }
}
pub fn decode(
    records: impl Iterator<Item = Value>,
    sessions: &mut BTreeMap<String, Session>,
    fallback: &str,
) -> Report {
    let mut report = Report {
        identity: fallback.into(),
        ..Report::default()
    };
    let mut run = Run::default();
    let mut session = fallback.to_owned();
    let mut any = false;
    for v in records {
        let kind = v["type"].as_str().unwrap_or_default();
        // New streams have host-owned boundaries. Init records and messages
        // after a result also identify appended invocations in older streams.
        // An init immediately following its host marker is the same run.
        if kind == "burrow.run" || (kind == "system" && v["subtype"] == "init") {
            let boundary = kind == "burrow.run" || run.initialized || !run.messages.is_empty();
            if boundary && run.active {
                partial(&mut run, &session, sessions, &mut report);
            }
            if kind == "burrow.run" {
                report.invocations.push(Invocation {
                    marked: true,
                    at: v["at"].as_i64(),
                    budget: v["budget"].as_i64(),
                    engine: v["engine"].as_str().map(str::to_owned),
                    model: v["model"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .map(str::to_owned),
                    ..Invocation::default()
                });
            }
            run.active = true;
            if kind == "system" {
                run.initialized = true;
                if let Some(m) = v["model"].as_str().filter(|m| !m.is_empty()) {
                    let inv = report.invocation();
                    if inv.model.is_none() {
                        inv.model = Some(m.to_owned());
                    }
                }
            }
        }
        if let Some(id) = v["session_id"].as_str() {
            session = id.into();
        }
        if kind == "assistant" {
            run.active = true;
            if let Some(m) = v["message"]["model"].as_str().filter(|m| !m.is_empty()) {
                let inv = report.invocation();
                if inv.model.is_none() {
                    inv.model = Some(m.to_owned());
                }
            }
            if let Some(id) = v["message"]["id"].as_str() {
                let known = run.messages.entry(id.into()).or_insert([0; 4]);
                for (old, new) in known
                    .iter_mut()
                    .zip(FIELDS.map(|k| v["message"]["usage"][k].as_u64().unwrap_or(0)))
                {
                    // Repeated blocks can omit fields already observed. Count
                    // one response once, retaining its known input/cache data.
                    *old = (*old).max(new);
                }
            }
        }
        if kind == "result" {
            any = true;
            let tokens = FIELDS.map(|k| v["usage"][k].as_u64().unwrap_or(0));
            let cost = v["total_cost_usd"].as_f64();
            let execution_crash = v["subtype"] == "error_during_execution";
            if v["usage"].is_object() && !(execution_crash && tokens == [0; 4]) {
                report.add(tokens);
                for (sum, value) in report.invocation().tokens.iter_mut().zip(tokens) {
                    *sum += value;
                }
            } else {
                partial_tokens(&mut run, &mut report);
            }
            if session != report.identity {
                report.invocation().session = Some(session.clone());
            }
            let previous = sessions.entry(session.clone()).or_default();
            if let Some(total) = cost
                && total >= previous.cost
                && !(execution_crash && total == 0.0)
            {
                let cost = (total - previous.cost).max(0.0);
                let had_gap = !previous.gaps.is_empty();
                report.prior_gap |= had_gap;
                let gaps = std::mem::take(&mut previous.gaps);
                previous.cost = total;
                report.cost += cost;
                report.recovered.extend(gaps.iter().cloned());
                let invocation = report.invocation();
                invocation.cost += cost;
                invocation.cumulative_cost = Some(total);
                invocation.prior_gap |= had_gap;
                invocation.recovered.extend(gaps);
            } else {
                // Missing or zeroed crash fields and regressed counters never
                // erase the last trustworthy cumulative total.
                report.cost_partial = true;
                report.invocation().cost_partial = true;
                missing(&session, sessions, &mut report);
            }
            run = Run::default();
        }
    }
    if run.active || !any {
        partial(&mut run, &session, sessions, &mut report);
    }
    report
}
#[cfg(test)]
mod tests;
