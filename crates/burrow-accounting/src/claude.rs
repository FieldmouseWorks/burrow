//! Claude Code's stream: result usage is per invocation; only cost is
//! cumulative across resumes of one session.
use super::{Report, Tokens};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ENGINE: &str = "claude";

pub const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// A session's last trustworthy cumulative cost, and the runs since that
/// reported none (the next total may include them).
#[derive(Clone, Debug, Default)]
pub struct Session {
    cost: f64,
    gaps: BTreeSet<String>,
}

/// One invocation, until its result.
#[derive(Clone, Debug, Default)]
struct Run {
    active: bool,
    initialized: bool,
    messages: BTreeMap<String, Tokens>,
}

/// One turn's stream, fed a record at a time.
#[derive(Clone, Debug)]
pub struct Decoder {
    report: Report,
    run: Run,
    session: String,
    any: bool,
}

impl Decoder {
    /// `identity` names the turn's missing runs, and its session until a
    /// record names one.
    pub fn new(identity: &str) -> Decoder {
        let mut report = Report::new(identity);
        report.cost = Some(0.0);
        Decoder {
            report,
            run: Run::default(),
            session: identity.into(),
            any: false,
        }
    }

    pub fn feed(&mut self, v: &Value, sessions: &mut BTreeMap<String, Session>) {
        let kind = v["type"].as_str().unwrap_or_default();
        let run = &mut self.run;
        let report = &mut self.report;
        // New streams have host-owned boundaries. Init records and messages
        // after a result also identify appended invocations in older streams.
        // An init immediately following its host marker is the same run.
        if kind == "burrow.run" || (kind == "system" && v["subtype"] == "init") {
            let boundary = kind == "burrow.run" || run.initialized || !run.messages.is_empty();
            if boundary && run.active {
                partial(run, &self.session, sessions, report);
            }
            run.active = true;
            if kind == "system" {
                run.initialized = true;
            }
        }
        if let Some(id) = v["session_id"].as_str() {
            self.session = id.into();
            report.sessions.insert(id.into());
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
                // A subagent's calls have a context of their own, and a
                // message no model call made (an error Claude Code writes
                // itself) reports none.
                let size = known[0] + known[2] + known[3];
                if v["parent_tool_use_id"].is_null() && size > 0 {
                    report.context(size);
                }
            }
        }
        if kind == "result" {
            self.any = true;
            let tokens = FIELDS.map(|k| v["usage"][k].as_u64().unwrap_or(0));
            let cost = v["total_cost_usd"].as_f64();
            let execution_crash = v["subtype"] == "error_during_execution";
            if v["usage"].is_object() && !(execution_crash && tokens == [0; 4]) {
                report.add(tokens);
            } else {
                partial_tokens(run, report);
            }
            let previous = sessions.entry(self.session.clone()).or_default();
            if let Some(total) = cost
                && total >= previous.cost
                && !(execution_crash && total == 0.0)
            {
                *report.cost.get_or_insert(0.0) += (total - previous.cost).max(0.0);
                report.prior_gap |= !previous.gaps.is_empty();
                report.recovered.append(&mut previous.gaps);
                previous.cost = total;
            } else {
                // Missing or zeroed crash fields and regressed counters never
                // erase the last trustworthy cumulative total.
                report.cost_partial = true;
                missing(&self.session, sessions, report);
            }
            *run = Run::default();
        }
    }

    /// The stream ended: a run without its result is missing.
    pub fn finish(mut self, sessions: &mut BTreeMap<String, Session>) -> Report {
        if self.run.active || !self.any {
            partial(&mut self.run, &self.session, sessions, &mut self.report);
        }
        self.report
    }

    /// What `finish` would count now, leaving every session as it is.
    pub fn snapshot(&self) -> Report {
        let mut report = self.report.clone();
        if self.run.active || !self.any {
            report.cost_partial = true;
            report.miss();
            partial_tokens(&mut self.run.clone(), &mut report);
        }
        report.unfinished = true;
        report
    }
}

fn partial(
    run: &mut Run,
    session: &str,
    sessions: &mut BTreeMap<String, Session>,
    report: &mut Report,
) {
    report.cost_partial = true;
    missing(session, sessions, report);
    partial_tokens(run, report);
    *run = Run::default();
}

fn missing(session: &str, sessions: &mut BTreeMap<String, Session>, report: &mut Report) {
    let id = report.miss();
    sessions.entry(session.into()).or_default().gaps.insert(id);
}

fn partial_tokens(run: &mut Run, report: &mut Report) {
    report.tokens_partial = true;
    for mut tokens in std::mem::take(&mut run.messages).into_values() {
        // Assistant output usage is a placeholder until the result arrives.
        tokens[1] = 0;
        report.add(tokens);
    }
}

/// A whole turn's stream at once.
pub fn decode(
    records: impl IntoIterator<Item = Value>,
    sessions: &mut BTreeMap<String, Session>,
    identity: &str,
) -> Report {
    let mut d = Decoder::new(identity);
    for v in records {
        d.feed(&v, sessions);
    }
    d.finish(sessions)
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
