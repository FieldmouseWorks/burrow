//! Codex's `turn.completed` usage is the thread's running total, not one
//! turn's: across resumed turns of one thread every count only grows, and a
//! run retried inside one turn repeats the earlier run's reasoning tokens.
//! So a turn counts its last total minus the thread's previous one, the way
//! `accounting` counts Claude's session cost. Codex streams carry no price.
use super::accounting::Report;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
#[derive(Default)]
pub struct Thread {
    total: [u64; 4],
    gaps: BTreeSet<String>,
}

/// A run ended without a total: its tokens are unknown here.
fn gap(thread: &str, threads: &mut BTreeMap<String, Thread>, report: &mut Report) {
    report.tokens_partial = true;
    let id = report.miss();
    threads.entry(thread.into()).or_default().gaps.insert(id);
}

/// One turn's stream; `identity` names its runs' gaps, and its thread
/// until a `thread.started` does.
pub fn decode(
    records: impl Iterator<Item = Value>,
    threads: &mut BTreeMap<String, Thread>,
    identity: &str,
) -> Report {
    let mut report = Report::new(identity);
    report.unpriced = true;
    let mut thread = identity.to_owned();
    // A turn under way, not yet completed.
    let mut open = false;
    let mut any = false;
    for v in records {
        match v["type"].as_str().unwrap_or_default() {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    thread = id.into();
                }
            }
            // A new run, or a new turn, while one is still open: that one
            // ended without its total.
            kind @ ("burrow.run" | "turn.started") => {
                if open {
                    gap(&thread, threads, &mut report);
                }
                open = kind == "turn.started";
            }
            "turn.failed" => {
                gap(&thread, threads, &mut report);
                open = false;
            }
            "turn.completed" => {
                any = true;
                open = false;
                let usage = &v["usage"];
                let total = FIELDS.map(|k| usage[k].as_u64().unwrap_or(0));
                let previous = threads.entry(thread.clone()).or_default();
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
                    gap(&thread, threads, &mut report);
                }
            }
            _ => {}
        }
    }
    if open || !any {
        gap(&thread, threads, &mut report);
    }
    report
}

#[cfg(test)]
mod tests;
