//! The engine a role's route runs a turn on now: its first engine, or, once
//! that one is short, the first engine after it along the route
//! (`ENGINE over PCT ENGINE ...`) with room. A script decides, each time a
//! turn starts or a run of it is refused, from the engines' meters and holds:
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

/// The machine's meters for one turn: Claude's reading (usage-reset's
/// claude.json) and its usage hold, the API credit's meter and its key, and
/// where the turn runs (a sandbox is given no key).
pub(crate) struct Live<'a> {
    pub cfg: &'a Config,
    pub sandboxed: bool,
    pub now: i64,
}

impl Live<'_> {
    /// A hold on a run of this turn on `engine`: the plan's usage or login
    /// for Claude, only the owner's pause for the API. A hold a fresh
    /// Claude meter proves over is none, as the next pass would release
    /// it ([`crate::usage::out_metered`]), so the subscription has its
    /// turns back as soon as its meter says so.
    fn held(&self, engine: Engine) -> bool {
        let run = Run {
            sandboxed: self.sandboxed,
            api: engine == Engine::ClaudeApi,
        };
        crate::usage::out_metered(self.cfg).is_some_and(|h| h.holds_run(run))
    }

    /// A key the credit's runs can launch with: none in a sandbox, and
    /// none missing, unreadable or empty ([`api::key`]).
    fn keyed(&self) -> bool {
        !self.sandboxed && api::key(self.cfg.claude_api_key_file.as_deref()).is_ok()
    }

    fn reading(&self, engine: Engine) -> Option<burrow_usage_reset::meter::Snapshot> {
        burrow_usage_reset::meter::Snapshot::read(
            self.cfg.meters.as_deref()?,
            engine.name(),
            self.now as f64,
        )
    }
}

impl Meters<Engine> for Live<'_> {
    fn short(&self, engine: Engine, pct: f64) -> bool {
        if self.held(engine) {
            return true;
        }
        match engine {
            Engine::Claude => self.reading(engine).is_some_and(|m| m.short(pct)),
            // No usable key (a sandbox is given none), or missing or
            // unreadable credit state: launch or admission would refuse it.
            Engine::ClaudeApi => {
                !self.keyed()
                    || api::short(
                        self.cfg.credit_file().as_deref(),
                        &self.cfg.jobs,
                        pct,
                        self.now,
                    )
            }
        }
    }

    fn room(&self, engine: Engine) -> bool {
        if self.held(engine) {
            return false;
        }
        match engine {
            Engine::Claude => self.reading(engine).is_some_and(|m| m.room()),
            Engine::ClaudeApi => {
                self.keyed()
                    && api::room(self.cfg.credit_file().as_deref(), &self.cfg.jobs, self.now)
            }
        }
    }
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
