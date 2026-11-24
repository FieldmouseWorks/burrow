//! The engine a role's route runs a turn on now: its first engine, or, once
//! that one is short, the first engine after it along the route
//! (`ENGINE over PCT ENGINE ...`) with room. A script decides, each time a
//! turn starts or a run of it is refused, from each engine's meter and hold:
//! never a model. Nothing here is remembered, so a turn goes back to the
//! first engine as soon as it isn't short.
//!
//! An engine is short at PCT when it's out of usage (a hold its refusal
//! recorded, which needs no reading) or its fresh reading is at PCT; it has
//! room when nothing holds its run and its fresh reading says so. A missing,
//! malformed, future or stale reading proves neither: the first engine runs
//! as it stands, and no turn is moved onto an engine it can't vouch for.

use super::{credit, parse_engine};
use crate::config::Config;
use crate::usage::login::Run;
use burrow_settings::roles::Route;

/// What a route asks of each engine.
pub(crate) trait Meters<E> {
    /// Proven short at PCT: out of usage, or a fresh reading at PCT.
    fn short(&self, engine: E, pct: f64) -> bool;
    /// Proven room for a run of this turn on it now.
    fn room(&self, engine: E) -> bool;
}

/// The engine of `first`, then each `(pct, engine)` hop, a turn runs on:
/// 1. the first engine not short at its own threshold (the next hop's PCT;
///    the last has none) that may run, the first engine by not being proven
///    short, each after it by proven room;
/// 2. else the first that may run at all: the first engine not proven out
///    of usage, or one after it with room;
/// 3. else the first engine, whose turn then waits for it as it always did.
pub(crate) fn pick<E: Copy>(first: E, over: &[(f64, E)], meters: &impl Meters<E>) -> E {
    let chain: Vec<(E, Option<f64>)> = std::iter::once(first)
        .chain(over.iter().map(|(_, e)| *e))
        .zip(over.iter().map(|(pct, _)| Some(*pct)).chain([None]))
        .collect();
    let runs = |i: usize, e: E| {
        if i == 0 {
            !meters.short(e, 100.0)
        } else {
            meters.room(e)
        }
    };
    let under = |e: E, pct: Option<f64>| pct.is_none_or(|p| !meters.short(e, p));
    chain
        .iter()
        .enumerate()
        .find(|(i, (e, pct))| runs(*i, *e) && under(*e, *pct))
        .or_else(|| chain.iter().enumerate().find(|(i, (e, _))| runs(*i, *e)))
        .map_or(first, |(_, (e, _))| *e)
}

/// The machine's per-engine meters and holds for one turn: each engine's own
/// meter snapshot (`<engine>.json`) and per-engine hold, plus the credit meter
/// for credit-metered engines, and whether the turn is sandboxed.
pub(crate) struct Live<'a> {
    pub cfg: &'a Config,
    pub sandboxed: bool,
    pub now: i64,
}

impl Live<'_> {
    /// Whether `engine` is held for this turn (discounting non-owner holds
    /// proven over by a fresh meter for that engine).
    fn held(&self, engine: &str) -> bool {
        let run = Run::new(engine, self.sandboxed);
        crate::usage::out_metered_for(self.cfg, &run).is_some()
    }

    /// A readable credential file for a credit-metered engine: none in a
    /// sandbox, and none missing, unreadable or empty.
    fn keyed(&self) -> bool {
        !self.sandboxed && credit::has_key(self.cfg.claude_api_key_file.as_deref())
    }

    fn reading(&self, engine: &str) -> Option<burrow_usage_reset::meter::Snapshot> {
        burrow_usage_reset::meter::Snapshot::read(
            self.cfg.meters.as_deref()?,
            engine,
            self.now as f64,
        )
    }
}

impl Meters<&str> for Live<'_> {
    fn short(&self, engine: &str, pct: f64) -> bool {
        if self.held(engine) {
            return true;
        }
        if self.cfg.uses_credit_meter(engine) {
            return !self.keyed()
                || credit::short(
                    self.cfg.credit_file().as_deref(),
                    &self.cfg.jobs,
                    pct,
                    self.now,
                );
        }
        self.reading(engine).is_some_and(|m| m.short(pct))
    }

    fn room(&self, engine: &str) -> bool {
        if self.held(engine) {
            return false;
        }
        if self.cfg.uses_credit_meter(engine) {
            return self.keyed()
                && credit::room(self.cfg.credit_file().as_deref(), &self.cfg.jobs, self.now);
        }
        self.reading(engine).is_some_and(|m| m.room())
    }
}

/// The engine name `route` selects now using arbitrary engine-name meters,
/// starting from `from` when pinned on the route: a pin the route doesn't name
/// runs alone.
pub(crate) fn pick_names<'a>(
    route: &'a Route,
    from: Option<&'a str>,
    meters: &impl Meters<&'a str>,
) -> &'a str {
    let mut chain: Vec<&'a str> = route.engines().collect();
    let mut pcts: Vec<f64> = route.over.iter().map(|h| h.pct).collect();
    if let Some(pin) = from {
        let Some(at) = chain.iter().position(|e| *e == pin) else {
            return pin;
        };
        chain.drain(..at);
        pcts.drain(..at);
    }
    let over: Vec<(f64, &'a str)> = pcts.into_iter().zip(chain[1..].iter().copied()).collect();
    pick(chain[0], &over, meters)
}

/// The engine ROUTE runs a turn on now, validating every engine name along the
/// route and starting from `from` when pinned on it.
pub(crate) fn engine<'a>(
    route: &'a Route,
    from: Option<&'a str>,
    meters: &impl Meters<&'a str>,
) -> Result<String, String> {
    for name in route.engines() {
        parse_engine(name)?;
    }
    if let Some(pin) = from {
        parse_engine(pin)?;
    }
    Ok(pick_names(route, from, meters).to_owned())
}

#[cfg(test)]
mod tests;
