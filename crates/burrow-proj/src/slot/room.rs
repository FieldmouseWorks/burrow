//! Whether a slot is free for work nobody waits on: what the drain asks
//! before it hands an issue on, and `proj why` says.

use std::fs;

use super::{Lock, line, lock};
use crate::config::Config;
use crate::engine::Engine;
use crate::usage;

/// Whether a turn on this engine started now would get a slot, as far as a
/// look from outside the line can tell: for Claude, no hold on its turn (in
/// a sandbox, `sandboxed`, or here), nobody in line a hold lets by and a slot
/// free past the pipeline's. Advisory: one on its way may take it first, and
/// the new one then waits in line like any other. For what work nobody waits
/// on (proj-inbox's drain) starts on idle slots. A fresh Claude meter releases
/// a hold it proves over first ([`usage::metered`]); [`seen_room`] is the same
/// look with nothing released or made.
pub fn room(cfg: &Config, engine: Engine, sandboxed: bool) -> bool {
    match engine {
        Engine::Claude => usage::metered(cfg),
    }
    fs::create_dir_all(cfg.jobs.join(".slots")).is_ok() && seen_room(cfg, engine, sandboxed)
}

/// [`room`], only reading: what an observer (`proj why`) says of the
/// drain's room.
pub fn seen_room(cfg: &Config, engine: Engine, sandboxed: bool) -> bool {
    // Only a slot seen free is room: one that can't be looked at isn't.
    let free = |dir: &str, n: usize| -> usize {
        let dir = cfg.jobs.join(dir);
        (1..=n)
            .filter(|i| matches!(lock(&dir.join(i.to_string())), Lock::Free | Lock::Missing))
            .count()
    };
    match engine {
        Engine::Claude => {
            // What the drain's `metered` would leave, released by nothing here.
            let hold = usage::out_metered(cfg);
            let held = |sandboxed| hold.as_ref().is_some_and(|h| h.holds_run(sandboxed));
            !held(sandboxed)
                && crate::retry::admission(cfg).is_ok_and(|p| p.is_empty())
                && line(cfg)
                    .iter()
                    .all(|w| held(usage::login::sandboxed(cfg, &w.job)))
                && free(".slots", cfg.max_jobs) > cfg.pipeline_slots
        }
    }
}
