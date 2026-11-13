//! Codex's `turn.completed` usage is the thread's running total, not one
//! turn's: across resumed turns of one thread every count only grows, and a
//! run retried inside one turn repeats the earlier run's reasoning tokens.
//! So a turn counts its last total minus the thread's previous one, the way
//! [`super::claude`] counts Claude's session cost. Codex's exec stream
//! carries no price and no per-call usage, so its turns have no measured
//! cost and no context occupancy.
use super::Report;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ENGINE: &str = "codex";

/// The usage fields as Codex writes them. Cached and cache-write tokens are
/// part of `input_tokens`, as reasoning is of `output_tokens`.
const FIELDS: [&str; 4] = [
    "input_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
];

/// A thread's last trustworthy total, and the runs since that reported
/// none (the next total may include them).
#[derive(Clone, Debug, Default)]
pub struct Thread {
    total: [u64; 4],
    gaps: BTreeSet<String>,
}

/// One turn's stream, fed a record at a time.
#[derive(Clone, Debug)]
pub struct Decoder {
    report: Report,
    thread: String,
    /// A turn under way, not yet completed.
    open: bool,
    any: bool,
}

/// A run ended without a total: its tokens are unknown here.
fn gap(thread: &str, threads: &mut BTreeMap<String, Thread>, report: &mut Report) {
    report.tokens_partial = true;
    let id = report.miss();
    threads.entry(thread.into()).or_default().gaps.insert(id);
}

impl Decoder {
    /// `identity` names the turn's missing runs, and its thread until a
    /// `thread.started` does.
    pub fn new(identity: &str) -> Decoder {
        Decoder {
            report: Report::new(identity),
            thread: identity.into(),
            open: false,
            any: false,
        }
    }

    pub fn feed(&mut self, v: &Value, threads: &mut BTreeMap<String, Thread>) {
        let report = &mut self.report;
        match v["type"].as_str().unwrap_or_default() {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    self.thread = id.into();
                    report.sessions.insert(id.into());
                }
            }
            // A new run, or a new turn, while one is still open: that one
            // ended without its total.
            kind @ ("burrow.run" | "turn.started") => {
                if self.open {
                    gap(&self.thread, threads, report);
                }
                self.open = kind == "turn.started";
            }
            "turn.failed" => {
                gap(&self.thread, threads, report);
                self.open = false;
            }
            "turn.completed" => {
                self.any = true;
                self.open = false;
                let usage = &v["usage"];
                let total = FIELDS.map(|k| usage[k].as_u64().unwrap_or(0));
                let previous = threads.entry(self.thread.clone()).or_default();
                if usage.is_object() && total.iter().zip(previous.total).all(|(t, p)| *t >= p) {
                    let d: [u64; 4] = std::array::from_fn(|i| total[i] - previous.total[i]);
                    let [input, cached, written, output] = d;
                    report.tokens[0] += input.saturating_sub(cached + written);
                    report.tokens[1] += output;
                    report.tokens[2] += cached;
                    report.tokens[3] += written;
                    report.prior_gap |= !previous.gaps.is_empty();
                    report.recovered.append(&mut previous.gaps);
                    previous.total = total;
                } else {
                    // A missing or regressed total never erases the last
                    // trustworthy one.
                    gap(&self.thread, threads, report);
                }
            }
            _ => {}
        }
    }

    /// The stream ended: a turn still open, or none completed, is missing.
    pub fn finish(mut self, threads: &mut BTreeMap<String, Thread>) -> Report {
        if self.open || !self.any {
            gap(&self.thread, threads, &mut self.report);
        }
        self.report
    }

    /// What `finish` would count now, leaving every thread as it is.
    pub fn snapshot(&self) -> Report {
        let mut report = self.report.clone();
        if self.open || !self.any {
            report.tokens_partial = true;
            report.miss();
        }
        report.unfinished = true;
        report
    }
}

/// A whole turn's stream at once.
pub fn decode(
    records: impl IntoIterator<Item = Value>,
    threads: &mut BTreeMap<String, Thread>,
    identity: &str,
) -> Report {
    let mut d = Decoder::new(identity);
    for v in records {
        d.feed(&v, threads);
    }
    d.finish(threads)
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
