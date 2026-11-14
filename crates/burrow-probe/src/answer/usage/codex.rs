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
    // `live`: an invocation (from its host boundary or its turn.started)
    // that has neither completed nor failed; `started`: its turn.started
    // was seen; `seen`: any invocation at all.
    let (mut live, mut started, mut seen) = (false, false, false);
    for v in records {
        match v["type"].as_str().unwrap_or_default() {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    thread = id.into();
                }
            }
            // A new invocation while one is still live: that one ended
            // without its total.
            "burrow.run" => {
                if live {
                    gap(&thread, threads, &mut report);
                }
                (live, started, seen) = (true, false, true);
            }
            // The live invocation's own turn, unless it has one already.
            "turn.started" => {
                if live && started {
                    gap(&thread, threads, &mut report);
                }
                (live, started, seen) = (true, true, true);
            }
            "turn.failed" => {
                gap(&thread, threads, &mut report);
                (live, started, seen) = (false, false, true);
            }
            "turn.completed" => {
                (live, started, seen) = (false, false, true);
                let usage = &v["usage"];
                let total = FIELDS.map(|k| usage[k].as_u64().unwrap_or(0));
                // Cache writes are optional; the other counters are not.
                let complete = usage.is_object()
                    && FIELDS
                        .iter()
                        .filter(|k| **k != "cache_write_input_tokens")
                        .all(|k| usage[*k].is_u64())
                    && (usage["cache_write_input_tokens"].is_null()
                        || usage["cache_write_input_tokens"].is_u64());
                let previous = threads.entry(thread.clone()).or_default();
                if complete && total.iter().zip(previous.total).all(|(t, p)| *t >= p) {
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
                    // A missing, partial or regressed total never erases the
                    // last trustworthy one.
                    gap(&thread, threads, &mut report);
                }
            }
            _ => {}
        }
    }
    if live || !seen {
        gap(&thread, threads, &mut report);
    }
    report
}

#[cfg(test)]
mod tests;
