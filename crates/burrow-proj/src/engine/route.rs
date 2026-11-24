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

use super::Engine;
use crate::config::Config;
use crate::usage::login::Run;
use burrow_engine_anthropic::api;
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
/// meter snapshot (`<engine>.json`) and per-engine hold, plus `claude-api`'s
/// credit meter and key check, and whether the turn is sandboxed.
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

    /// A key the credit's runs can launch with: none in a sandbox, and
    /// none missing, unreadable or empty ([`api::key`]).
    fn keyed(&self) -> bool {
        !self.sandboxed && api::key(self.cfg.claude_api_key_file.as_deref()).is_ok()
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
        if engine == burrow_settings::credit::ENGINE {
            return !self.keyed()
                || api::short(
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
        if engine == burrow_settings::credit::ENGINE {
            return self.keyed()
                && api::room(self.cfg.credit_file().as_deref(), &self.cfg.jobs, self.now);
        }
        self.reading(engine).is_some_and(|m| m.room())
    }
}

impl Meters<Engine> for Live<'_> {
    fn short(&self, engine: Engine, pct: f64) -> bool {
        Meters::<&str>::short(self, engine.name(), pct)
    }

    fn room(&self, engine: Engine) -> bool {
        Meters::<&str>::room(self, engine.name())
    }
}

/// The engine name `route` selects now using arbitrary engine-name meters,
/// starting from `from` when pinned on the route.
#[cfg(test)]
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

/// The engine ROUTE runs a turn on now, starting from `from` when the turn
/// is pinned to an engine on it: a pin the route doesn't name runs alone.
pub(crate) fn engine(
    route: &Route,
    from: Option<Engine>,
    meters: &impl Meters<Engine>,
) -> Result<Engine, String> {
    let mut chain = route
        .engines()
        .map(Engine::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let mut pcts: Vec<f64> = route.over.iter().map(|h| h.pct).collect();
    if let Some(pin) = from {
        let Some(at) = chain.iter().position(|e| *e == pin) else {
            return Ok(pin);
        };
        chain.drain(..at);
        pcts.drain(..at);
    }
    let over: Vec<(f64, Engine)> = pcts.into_iter().zip(chain[1..].iter().copied()).collect();
    Ok(pick(chain[0], &over, meters))
}

#[cfg(test)]
mod tests;
