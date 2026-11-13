//! The usage selector: which of a role's ordered choices (engine, model,
//! effort) a turn runs on, from the engines' live meters
//! (burrow_usage_reset::meter), their holds and what's installed. A plain
//! rule, no model deciding, and the same inputs always give the same
//! [`Decision`]:
//!
//! - A choice whose engine isn't installed, or is on a hold, is never taken.
//! - Plan before credits: the first choice whose fresh reading has every
//!   window under its engine's threshold runs on the plan, wherever it sits
//!   in the list. Only when none has, and the role allows credits, does the
//!   first choice whose fresh reading has credits run on them.
//! - A missing, unreadable, malformed, future or stale reading moves
//!   nothing: a first usable choice with one runs as it stands (unmetered),
//!   and once usage has passed a choice over, one with no fresh reading is
//!   never moved onto, since nothing says the move helps.
//!
//! The decision keeps the outcome, how it's funded, why each choice before
//! it was passed over and the meter evidence for every engine the list
//! names: what a turn persists and what degradation reports read. Nothing
//! here routes a turn yet; launch integration is #747's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use burrow_usage_reset::meter::{Snapshot, Unfit};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::engine::Engine;

/// One choice in a role's ordered list: an engine, and the model and effort
/// to run it with (none keeps the engine's own).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preference {
    pub engine: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

/// The engines a choice may name: those the installed plugin declarations
/// declare, when there are any, otherwise the two built-in engines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed(BTreeSet<String>);

impl Installed {
    /// Claude and Codex, the built-in engines.
    pub fn builtin() -> Self {
        Self(
            [Engine::Claude, Engine::Codex]
                .map(|e| e.name().to_owned())
                .into(),
        )
    }

    /// The engines the installed plugin declarations name, or the built-ins
    /// when there are none to read.
    pub fn declared<S: Into<String>>(declarations: Option<impl IntoIterator<Item = S>>) -> Self {
        declarations.map_or_else(Self::builtin, |d| {
            Self(d.into_iter().map(Into::into).collect())
        })
    }

    pub fn has(&self, engine: &str) -> bool {
        self.0.contains(engine)
    }

    pub fn engines(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// A role's usage policy: each engine's threshold, the percent at which a
/// window has no plan headroom left (100 for an engine not given one), and
/// whether the role may spend credits.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    thresholds: BTreeMap<String, f64>,
    credits: bool,
}

impl Policy {
    pub fn new(thresholds: BTreeMap<String, f64>, credits: bool) -> Result<Self, String> {
        if let Some((engine, pct)) = thresholds
            .iter()
            .find(|(_, pct)| !(0.0..=100.0).contains(*pct))
        {
            return Err(format!("{engine}: threshold {pct} must be 0 to 100"));
        }
        Ok(Self {
            thresholds,
            credits,
        })
    }

    pub fn threshold(&self, engine: &str) -> f64 {
        self.thresholds.get(engine).copied().unwrap_or(100.0)
    }

    pub fn credits(&self) -> bool {
        self.credits
    }
}

/// Work held back on an engine (Claude's usage hold), until when and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    pub until: i64,
    pub reason: String,
    /// An explicit owner pause rather than a refused run.
    pub owner: bool,
}

/// An engine's meter as the selector saw it: the fresh reading, or why
/// there's none that can move a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Evidence {
    Fresh { reading: Snapshot },
    Unfit { why: Unfit },
}

impl Evidence {
    /// ENGINE's meter in DIR at NOW; no meters dir is no reading.
    pub fn read(dir: Option<&Path>, engine: &str, now: i64) -> Self {
        match dir.map(|d| Snapshot::check(d, engine, now as f64)) {
            Some(Ok(reading)) => Self::Fresh { reading },
            Some(Err(why)) => Self::Unfit { why },
            None => Self::Unfit {
                why: Unfit::Missing,
            },
        }
    }
}

/// Everything a selection reads besides the role's list and policy.
#[derive(Clone, Debug, PartialEq)]
pub struct Inputs {
    pub installed: Installed,
    /// Active holds by engine.
    pub holds: BTreeMap<String, Hold>,
    /// Meter evidence by engine; an engine with none is missing.
    pub meters: BTreeMap<String, Evidence>,
    pub now: i64,
}

impl Inputs {
    /// Each installed engine's meter in DIR at NOW, with HOLDS.
    pub fn read(
        installed: Installed,
        holds: BTreeMap<String, Hold>,
        dir: Option<&Path>,
        now: i64,
    ) -> Self {
        let meters = installed
            .engines()
            .map(|e| (e.to_owned(), Evidence::read(dir, e, now)))
            .collect();
        Self {
            installed,
            holds,
            meters,
            now,
        }
    }

    /// The machine as it stands: its meters dir, and Claude's usage hold.
    pub fn live(cfg: &Config, installed: Installed) -> Self {
        let holds = crate::usage::out(cfg)
            .map(|s| {
                (
                    Engine::Claude.name().to_owned(),
                    Hold {
                        until: s.until,
                        reason: s.said,
                        owner: s.owner,
                    },
                )
            })
            .into_iter()
            .collect();
        Self::read(installed, holds, cfg.meters.as_deref(), crate::now())
    }

    fn meter(&self, engine: &str) -> Evidence {
        self.meters.get(engine).cloned().unwrap_or(Evidence::Unfit {
            why: Unfit::Missing,
        })
    }
}

/// What the turn's run is paid from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Funding {
    /// The plan, with headroom under the engine's threshold.
    Plan,
    /// Credits, every plan in the list being out.
    Credits,
    /// Unknown: no fresh reading, so usage moved nothing.
    Unmetered,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Outcome {
    /// Run CHOICE, the list's INDEXth (from 0).
    Selected {
        index: usize,
        choice: Preference,
        funding: Funding,
    },
    /// No choice may run now.
    Blocked,
}

/// Why a choice was passed over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reason {
    /// Its engine isn't installed.
    Unavailable,
    /// Its engine is on a hold.
    Held { hold: Hold },
    /// A window at or past its engine's threshold: no plan headroom. Credits
    /// says whether the reading had credits to carry on with.
    Threshold {
        window: String,
        used: f64,
        threshold: f64,
        credits: bool,
    },
    /// Usage moved the turn on, and this engine has no fresh reading to say
    /// it has room (the decision's meters say why).
    Unmetered,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub index: usize,
    pub choice: Preference,
    #[serde(flatten)]
    pub reason: Reason,
}

/// One selection, whole: what to persist with the turn and report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// Whether the role allowed credits.
    pub credits_permitted: bool,
    /// The choices passed over, in list order, each with why.
    pub skipped: Vec<Skipped>,
    /// The meter evidence for each engine the list names.
    pub meters: BTreeMap<String, Evidence>,
    pub at: i64,
}

impl Decision {
    /// Not the first choice on its plan (or as it stands, unmetered):
    /// another choice, credits, or nothing at all.
    pub fn degraded(&self) -> bool {
        !matches!(
            self.outcome,
            Outcome::Selected {
                index: 0,
                funding: Funding::Plan | Funding::Unmetered,
                ..
            }
        )
    }
}

/// The window furthest past THRESHOLD, if any is at or past it; the first by
/// name among equals.
fn spent(reading: &Snapshot, threshold: f64) -> Option<(&String, f64)> {
    reading
        .windows
        .iter()
        .filter(|(_, w)| w.used >= threshold)
        .map(|(name, w)| (name, w.used))
        .fold(None, |best, (name, used)| match best {
            Some((_, b)) if b >= used => best,
            _ => Some((name, used)),
        })
}

/// Select from PREFERENCES under POLICY with INPUTS: see the module's rule.
pub fn select(preferences: &[Preference], policy: &Policy, inputs: &Inputs) -> Decision {
    let meters: BTreeMap<String, Evidence> = preferences
        .iter()
        .map(|p| (p.engine.clone(), inputs.meter(&p.engine)))
        .collect();
    let decide = |outcome, skipped| Decision {
        outcome,
        credits_permitted: policy.credits(),
        skipped,
        meters: meters.clone(),
        at: inputs.now,
    };
    let selected = |index: usize, funding| Outcome::Selected {
        index,
        choice: preferences[index].clone(),
        funding,
    };
    let mut skipped = Vec::new();
    // Usage has passed a choice over: what follows is a move.
    let mut moved = false;
    let mut on_credits = None;
    for (index, choice) in preferences.iter().enumerate() {
        let engine = choice.engine.as_str();
        let reason = if !inputs.installed.has(engine) {
            Reason::Unavailable
        } else if let Some(hold) = inputs.holds.get(engine) {
            moved = true;
            Reason::Held { hold: hold.clone() }
        } else {
            match &meters[engine] {
                Evidence::Fresh { reading } => {
                    let threshold = policy.threshold(engine);
                    let Some((window, used)) = spent(reading, threshold) else {
                        return decide(selected(index, Funding::Plan), skipped);
                    };
                    moved = true;
                    if reading.credits && on_credits.is_none() {
                        on_credits = Some(index);
                    }
                    Reason::Threshold {
                        window: window.clone(),
                        used,
                        threshold,
                        credits: reading.credits,
                    }
                }
                Evidence::Unfit { .. } if !moved => {
                    return decide(selected(index, Funding::Unmetered), skipped);
                }
                Evidence::Unfit { .. } => Reason::Unmetered,
            }
        };
        skipped.push(Skipped {
            index,
            choice: choice.clone(),
            reason,
        });
    }
    match on_credits.filter(|_| policy.credits()) {
        Some(index) => {
            skipped.retain(|s| s.index != index);
            decide(selected(index, Funding::Credits), skipped)
        }
        None => decide(Outcome::Blocked, skipped),
    }
}

#[cfg(test)]
mod tests;
