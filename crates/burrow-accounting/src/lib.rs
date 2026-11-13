//! What a job's turns used, read from its engines' streams: one home for the
//! accounting proj and the probe's usage answer share.
//!
//! The streams say different things, and each is kept for what it is:
//!
//! - Claude Code's stream-json reports each model call's usage on its
//!   assistant message, each invocation's summed usage on its `result`, and
//!   the session's cumulative cost (`total_cost_usd`) there too. So tokens
//!   count per invocation, cost as the session's growth since its last
//!   trustworthy total ([`claude`]), and context occupancy is the prompt of
//!   the latest main-conversation call: input plus cache read and written.
//! - Codex's `exec --json` reports only the thread's running token totals on
//!   `turn.completed`: no cost and no per-call usage, so a turn counts its
//!   thread's growth ([`codex`]) and context occupancy is unavailable.
//!
//! Context occupancy is how big the session's prompt is now; tokens are the
//! traffic summed over every call. Money is measured (the engine's own
//! counter), estimated (configured [`Prices`] times tokens, for an engine
//! whose stream carries no cost) or unavailable, and the three never mix.
//!
//! A [`Job`] replays its turns in order, so resumed sessions and threads
//! count only what each turn added, a run that reported no total is kept as
//! a missing run with an identity until a later total recovers it, and a
//! turn still being written is a snapshot that agrees with its complete
//! replay. [`Tail`] reads a growing stream incrementally.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub mod claude;
pub mod codex;
mod price;
mod tail;
pub use price::{Estimate, Price, Prices, describe as describe_prices};
pub use tail::Tail;

/// Token traffic, summed over every model call: input (uncached), output,
/// cache read and cache written.
pub type Tokens = [u64; 4];

/// One turn's usage, as its stream says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub tokens: Tokens,
    /// Measured money: what the engine's cumulative counter grew by. None
    /// where the engine's stream carries no cost.
    pub cost: Option<f64>,
    pub tokens_partial: bool,
    /// Some run's cost is unknown: `cost` is a lower bound.
    pub cost_partial: bool,
    /// The total this turn's cost (or, unpriced, tokens) came from may
    /// include earlier runs that reported none.
    pub prior_gap: bool,
    /// Run identities, so aggregates can match a missing run to the later
    /// total that recovers it.
    pub missing: BTreeSet<String>,
    pub recovered: BTreeSet<String>,
    /// Context occupancy after the turn's latest main-conversation call, and
    /// its largest; None where the stream doesn't report calls.
    pub context: Option<u64>,
    pub context_peak: Option<u64>,
    /// The sessions (Claude) or threads (Codex) the turn's runs used.
    pub sessions: BTreeSet<String>,
    /// The stream hasn't ended: a snapshot as of its last complete record.
    pub unfinished: bool,
    identity: String,
}

impl Report {
    pub fn new(identity: &str) -> Report {
        Report {
            identity: identity.into(),
            ..Report::default()
        }
    }

    /// A run whose usage went unreported: its identity, kept by the
    /// session or thread until a later total recovers it.
    pub fn miss(&mut self) -> String {
        let id = format!("{}:{}", self.identity, self.missing.len());
        self.missing.insert(id.clone());
        id
    }

    fn add(&mut self, tokens: Tokens) {
        for (sum, value) in self.tokens.iter_mut().zip(tokens) {
            *sum += value;
        }
    }

    fn context(&mut self, size: u64) {
        self.context = Some(size);
        self.context_peak = Some(self.context_peak.unwrap_or(0).max(size));
    }

    /// One line: tokens, money, context, and what's incomplete.
    pub fn show(&self, estimate: Option<&Estimate>) -> String {
        let mut s = format!(
            "input={} output={} cache-read={} cache-write={}",
            self.tokens[0], self.tokens[1], self.tokens[2], self.tokens[3]
        );
        match (self.cost, estimate) {
            (Some(cost), _) => {
                let label = if self.cost_partial {
                    "known-cost"
                } else {
                    "cost"
                };
                s += &format!(" {label}=${cost:.6}");
            }
            (None, Some(e)) => s += &format!(" estimate=${:.6} ({})", e.usd, e.basis),
            (None, None) => s += " cost unpriced",
        }
        match self.context {
            Some(c) => {
                s += &format!(" context={c} peak={}", self.context_peak.unwrap_or(c));
            }
            None => s += " context unavailable",
        }
        let unpriced = self.cost.is_none();
        if self.tokens_partial {
            s += if unpriced {
                "; partial tokens (a run reported no total)"
            } else {
                "; partial tokens (output counts completed runs only)"
            };
        }
        if self.cost_partial {
            s += "; additional cost unavailable";
        }
        if self.prior_gap {
            s += if unpriced {
                "; tokens may include unreported earlier runs"
            } else {
                "; cost may include unreported earlier runs"
            };
        }
        if self.unfinished {
            s += "; unfinished (no outcome yet: counts to its last complete record)";
        }
        s
    }
}

/// What the ledger knows of a turn before reading its stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// Names the turn's missing runs: unique across jobs (`JOB/N`).
    pub identity: String,
    pub engine: String,
    /// The model it ran on; None for the engine's own.
    pub model: Option<String>,
    /// Its first start, Unix seconds; None where it wasn't recorded.
    pub started: Option<i64>,
}

/// A turn as the ledger keeps it. `report` is None for an engine whose
/// stream isn't read here.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub meta: Meta,
    pub report: Option<Report>,
    /// Configured estimate, only where the stream carries no measured cost.
    pub estimate: Option<Estimate>,
}

enum Decoder {
    Claude(claude::Decoder),
    Codex(codex::Decoder),
    Unread,
}

/// One job's turns, replayed in order. Feed records as they arrive; a
/// snapshot never changes what a complete replay would count.
#[derive(Default)]
pub struct Job {
    prices: Prices,
    sessions: BTreeMap<String, claude::Session>,
    threads: BTreeMap<String, codex::Thread>,
    turns: Vec<Turn>,
    open: Option<(Meta, Decoder)>,
}

impl Job {
    pub fn new(prices: Prices) -> Job {
        Job {
            prices,
            ..Job::default()
        }
    }

    /// The next turn starts: any turn still open ends first.
    pub fn begin(&mut self, meta: Meta) {
        self.end();
        let decoder = match meta.engine.as_str() {
            claude::ENGINE => Decoder::Claude(claude::Decoder::new(&meta.identity)),
            codex::ENGINE => Decoder::Codex(codex::Decoder::new(&meta.identity)),
            _ => Decoder::Unread,
        };
        self.open = Some((meta, decoder));
    }

    /// One record of the open turn's stream.
    pub fn feed(&mut self, record: &Value) {
        match self.open.as_mut().map(|(_, d)| d) {
            Some(Decoder::Claude(d)) => d.feed(record, &mut self.sessions),
            Some(Decoder::Codex(d)) => d.feed(record, &mut self.threads),
            _ => {}
        }
    }

    /// The open turn's stream ended: an unfinished run in it is missing.
    pub fn end(&mut self) {
        let Some((meta, decoder)) = self.open.take() else {
            return;
        };
        let report = match decoder {
            Decoder::Claude(d) => Some(d.finish(&mut self.sessions)),
            Decoder::Codex(d) => Some(d.finish(&mut self.threads)),
            Decoder::Unread => None,
        };
        self.turns.push(self.turn(meta, report));
    }

    /// The open turn as it stands, marked unfinished.
    pub fn snapshot(&self) -> Option<Turn> {
        let (meta, decoder) = self.open.as_ref()?;
        let report = match decoder {
            Decoder::Claude(d) => Some(d.snapshot()),
            Decoder::Codex(d) => Some(d.snapshot()),
            Decoder::Unread => None,
        };
        Some(self.turn(meta.clone(), report))
    }

    fn turn(&self, meta: Meta, report: Option<Report>) -> Turn {
        let estimate = report
            .as_ref()
            .filter(|r| r.cost.is_none())
            .and_then(|r| price::estimate(&self.prices, &meta, &r.tokens));
        Turn {
            meta,
            report,
            estimate,
        }
    }

    /// The latest turn: the open one's snapshot, or the last to end.
    pub fn last(&self) -> Option<Turn> {
        self.snapshot().or_else(|| self.turns.last().cloned())
    }

    /// Every turn so far, the open one's snapshot last.
    pub fn turns(&self) -> Vec<Turn> {
        let mut all = self.turns.clone();
        all.extend(self.snapshot());
        all
    }

    /// The job's lifetime: every turn, the open one's snapshot included.
    pub fn totals(&self) -> Tally {
        let mut t = Tally::default();
        for turn in self.turns() {
            t.add_turn(&turn);
        }
        t
    }
}

/// What a group of turns adds up to. Sums of per-turn deltas, so a total
/// never counts a cumulative counter twice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tally {
    pub turns: u64,
    pub unfinished: u64,
    pub tokens: Tokens,
    /// Measured money, and the turns that had a measured counter.
    pub cost: f64,
    pub measured: u64,
    /// Measured turns some of whose cost is unknown.
    pub cost_partial: u64,
    /// Estimated money, the turns it covers and the price keys it used.
    pub estimate: f64,
    pub estimated: u64,
    pub bases: BTreeSet<String>,
    /// Turns with tokens but neither measured cost nor an estimate.
    pub unpriced: u64,
    /// Turns whose engine's stream isn't read: no tokens or cost.
    pub unread: u64,
    pub tokens_partial: u64,
    pub missing: BTreeSet<String>,
    pub recovered: BTreeSet<String>,
    pub sessions: BTreeSet<String>,
    /// Context after the latest turn added (None where its stream doesn't
    /// say), and the largest of any turn.
    pub context: Option<u64>,
    pub context_peak: Option<u64>,
}

impl Tally {
    /// Turns must come in order for `context` to be the latest.
    pub fn add_turn(&mut self, turn: &Turn) {
        self.turns += 1;
        let Some(r) = &turn.report else {
            self.unread += 1;
            self.context = None;
            return;
        };
        self.unfinished += u64::from(r.unfinished);
        for (sum, v) in self.tokens.iter_mut().zip(r.tokens) {
            *sum += v;
        }
        match (r.cost, &turn.estimate) {
            (Some(cost), _) => {
                self.cost += cost;
                self.measured += 1;
                self.cost_partial += u64::from(r.cost_partial);
            }
            (None, Some(e)) => {
                self.estimate += e.usd;
                self.estimated += 1;
                self.bases.insert(e.basis.clone());
            }
            (None, None) => self.unpriced += 1,
        }
        self.tokens_partial += u64::from(r.tokens_partial);
        self.missing.extend(r.missing.iter().cloned());
        self.recovered.extend(r.recovered.iter().cloned());
        self.sessions.extend(r.sessions.iter().cloned());
        self.context = r.context;
        self.context_peak = self.context_peak.max(r.context_peak);
    }

    /// Two groups together; `context` keeps the later group's.
    pub fn add(&mut self, other: &Tally) {
        self.turns += other.turns;
        self.unfinished += other.unfinished;
        for (sum, v) in self.tokens.iter_mut().zip(other.tokens) {
            *sum += v;
        }
        self.cost += other.cost;
        self.measured += other.measured;
        self.cost_partial += other.cost_partial;
        self.estimate += other.estimate;
        self.estimated += other.estimated;
        self.bases.extend(other.bases.iter().cloned());
        self.unpriced += other.unpriced;
        self.unread += other.unread;
        self.tokens_partial += other.tokens_partial;
        self.missing.extend(other.missing.iter().cloned());
        self.recovered.extend(other.recovered.iter().cloned());
        self.sessions.extend(other.sessions.iter().cloned());
        self.context = other.context;
        self.context_peak = self.context_peak.max(other.context_peak);
    }

    /// Runs whose cost (tokens, unpriced) is still missing.
    pub fn still_missing(&self) -> usize {
        self.missing.difference(&self.recovered).count()
    }
    /// Missing runs a later total in the group recovered.
    pub fn recovered_here(&self) -> usize {
        self.recovered.intersection(&self.missing).count()
    }
    /// Recovered runs from outside the group.
    pub fn recovered_outside(&self) -> usize {
        self.recovered.difference(&self.missing).count()
    }

    /// Money, each kind named: measured, estimated, unavailable.
    pub fn money(&self) -> String {
        let mut parts = Vec::new();
        if self.measured > 0 {
            // A lower bound only while some run's cost is still missing: a
            // later total in the group that recovered it made it whole.
            let label = if self.cost_partial > 0 && self.still_missing() > 0 {
                "known-cost"
            } else {
                "cost"
            };
            parts.push(format!("{label}=${:.2}", self.cost));
        }
        if self.estimated > 0 {
            parts.push(format!(
                "estimate=${:.2} ({})",
                self.estimate,
                self.bases.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        let none = self.unpriced + self.unread;
        if none > 0 {
            parts.push(format!("{none} turns cost unavailable"));
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests;
