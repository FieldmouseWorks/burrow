//! Result usage is per invocation; only cost is cumulative across resumes.
use serde_json::Value;
use std::collections::BTreeMap;
pub const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];
#[derive(Default)]
pub struct Session {
    cost: f64,
    gap: bool,
}
#[derive(Default)]
pub struct Report {
    pub tokens: [u64; 4],
    pub cost: f64,
    pub tokens_partial: bool,
    pub cost_partial: bool,
    pub prior_gap: bool,
}
impl Report {
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
    sessions.entry(session.into()).or_default().gap = true;
    partial_tokens(run, report);
    *run = Run::default();
}
fn partial_tokens(run: &mut Run, report: &mut Report) {
    report.tokens_partial = true;
    for mut tokens in std::mem::take(&mut run.messages).into_values() {
        // Assistant output usage is a placeholder until the result arrives.
        tokens[1] = 0;
        report.add(tokens);
    }
}
pub fn decode(
    records: impl Iterator<Item = Value>,
    sessions: &mut BTreeMap<String, Session>,
    fallback: &str,
) -> Report {
    let mut report = Report::default();
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
            run.active = true;
            if kind == "system" {
                run.initialized = true;
            }
        }
        if let Some(id) = v["session_id"].as_str() {
            session = id.into();
        }
        if kind == "assistant" {
            run.active = true;
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
            } else {
                partial_tokens(&mut run, &mut report);
            }
            let previous = sessions.entry(session.clone()).or_default();
            if let Some(total) = cost
                && total >= previous.cost
                && !(execution_crash && total == 0.0)
            {
                report.cost += (total - previous.cost).max(0.0);
                report.prior_gap |= previous.gap;
                previous.cost = total;
                previous.gap = false;
            } else {
                // Missing or zeroed crash fields and regressed counters never
                // erase the last trustworthy cumulative total.
                report.cost_partial = true;
                previous.gap = true;
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
