//! Whether a slot is free for work nobody waits on: what the drain asks
//! before it hands an issue on, and `proj why` says.

use std::fs;

use burrow_engine_anthropic::api;

use super::{Lock, line, lock};
use crate::config::Config;
use crate::engine::Engine;
use crate::usage::{self, login::Run};

/// Whether a turn on this engine started now would get a slot, as far as a
/// look from outside the line can tell: no hold blocking that engine's run,
/// nobody in line whose own engine isn't held, and a slot free past the
/// pipeline's. A fresh meter for that engine releases a non-owner hold it
/// proves over first ([`usage::metered_engine`]); [`seen_room`] is the same
/// read-only look with nothing released or made.
pub fn room(cfg: &Config, engine: Engine, sandboxed: bool) -> bool {
    usage::metered_engine(cfg, engine.name());
    fs::create_dir_all(cfg.jobs.join(".slots")).is_ok() && seen_room(cfg, engine, sandboxed)
}

/// [`room`], only reading: what an observer (`proj why`) says of the
/// drain's room.
pub fn seen_room(cfg: &Config, engine: Engine, sandboxed: bool) -> bool {
    if engine == Engine::ClaudeApi
        && (sandboxed || !api::room(cfg.credit_file().as_deref(), &cfg.jobs, crate::now()))
    {
        return false;
    }
    seen_room_engine(cfg, engine.name(), sandboxed)
}

/// Engine-name room check without provider-specific enum dispatch: checks
/// per-engine holds (including fresh meter recovery), waiting retries/line on
/// unheld engines, and free shared slots past the pipeline reserve.
pub(crate) fn seen_room_engine(cfg: &Config, engine: &str, sandboxed: bool) -> bool {
    let free = |dir: &str, n: usize| -> usize {
        let dir = cfg.jobs.join(dir);
        (1..=n)
            .filter(|i| matches!(lock(&dir.join(i.to_string())), Lock::Free | Lock::Missing))
            .count()
    };
    let held = |run: &Run| usage::out_metered_for(cfg, run).is_some();
    let this = Run::new(engine, sandboxed);
    !held(&this)
        && crate::retry::admission(cfg).is_ok_and(|p| {
            p.iter()
                .all(|(id, _, _)| held(&usage::login::run_of(cfg, id)))
        })
        && line(cfg)
            .iter()
            .all(|w| held(&usage::login::run_of(cfg, &w.job)))
        && free(".slots", cfg.max_jobs) > cfg.pipeline_slots
}
