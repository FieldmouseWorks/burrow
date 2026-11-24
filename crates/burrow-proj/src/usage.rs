//! Per-engine usage holds, expired-login notices, and meter recovery.
//!
//! Each engine owns its own hold entry in `jobs/.usage-holds.json` and its own
//! `HOST-notify` notice key (`proj-usage-<engine>`), while `*` represents an
//! explicit owner pause over every engine (`proj-usage`). Any `claude` or `*`
//! hold is mirrored to `jobs/.usage.json` in strict pre-change format during
//! deploy overlap so retained pre-change turns stay fenced and never lose a
//! refusal. A quota or login refusal on engine A holds only engine A, leaving
//! engine B free to drain slots and allowing ordered failover routes
//! (`over PCT ENGINE`) to move turns to the next engine with room.

use std::collections::BTreeMap;
use std::fs;

use crate::config::Config;
use crate::job::{Job, kick};
use crate::record::TurnFile;
use crate::waits::rewatch_all;
use crate::{Fail, mtime, now};

pub(crate) mod login;
mod notice;
mod store;

pub use notice::{hold_line, hold_line_for, line, policy, when};
#[cfg(test)]
pub(crate) use store::LEGACY_FILE;
#[cfg(test)]
use store::{ADMISSION, LEGACY_ADMISSION, LEGACY_RELEASED};
pub use store::{ALL_ENGINES, DEFAULT_ENGINE, Spent, Store};
pub(crate) use store::{FILE, lock, try_lock};

/// How long an engine is held when a refusal omits a reset time or names one in the past.
const UNSAID: i64 = 3600;
/// How often an open notice is reminded while the hold stands.
const RETELL: i64 = 3600;
/// Backoff for `proj-watch` when tending fails.
const RETRY: i64 = RETELL / 12;

/// What an earlier proj's own record of a usage limit is said as once adopted.
pub const EARLIER: &str = "a usage limit an earlier proj or watchdog recorded";

/// All active holds keyed by engine name (`*` for a global owner pause).
pub fn all_out(cfg: &Config) -> BTreeMap<String, Spent> {
    held_all(cfg).unwrap_or_default()
}

/// All active holds keyed by engine name, returning an error if `.usage.json`
/// cannot be read or parsed (for `proj settings show`).
pub fn held_all(cfg: &Config) -> Result<BTreeMap<String, Spent>, String> {
    let now = now();
    let store = store::read_store(cfg)?;
    Ok(store
        .holds
        .into_iter()
        .filter(|(_, s)| s.holds(now))
        .collect())
}

/// The active hold blocking `run` now, if any.
pub fn out_for(cfg: &Config, run: &login::Run) -> Option<Spent> {
    let store = store::read_store(cfg).ok()?;
    store.hold_for(run, now()).cloned()
}

/// The active hold on `claude` (or global `*` owner pause).
pub fn out(cfg: &Config) -> Option<Spent> {
    out_for(cfg, &login::Run::plan(false))
}

/// Read-only hold check on `claude` (or global `*`), propagating read errors.
pub fn held(cfg: &Config) -> Result<Option<Spent>, String> {
    let store = store::read_store(cfg)?;
    Ok(store.hold_for(&login::Run::plan(false), now()).cloned())
}

/// Read the per-engine store, falling back to an empty store on read error.
pub(crate) fn read_store_ok(cfg: &Config) -> Store {
    store::read_store(cfg).unwrap_or_default()
}

/// Any kept record (holding or recently released) applying to `run`.
pub fn kept_for(cfg: &Config, run: &login::Run) -> Option<Spent> {
    let store = store::read_store(cfg).ok()?;
    store.kept_for(run).cloned()
}

/// Any kept record applying to `claude` (or global `*`).
pub fn kept(cfg: &Config) -> Option<Spent> {
    kept_for(cfg, &login::Run::plan(false))
}

/// Release any non-owner engine holds whose fresh meter reading after the last
/// refusal proves room is back.
pub fn metered(cfg: &Config) {
    let Ok(store) = store::read_store(cfg) else {
        return;
    };
    let now = now();
    for (engine, s) in store.holds {
        if engine == ALL_ENGINES || s.owner || !s.holds(now) {
            continue;
        }
        if let Some(started) = meter_proves(cfg, &engine, &s) {
            end_engine(cfg, &engine, Some(started), false);
        }
    }
}

/// Release `engine`'s non-owner hold if a fresh meter reading for `engine` proves room.
pub fn metered_engine(cfg: &Config, engine: &str) {
    if engine == ALL_ENGINES {
        return;
    }
    let Ok(store) = store::read_store(cfg) else {
        return;
    };
    let Some(s) = store
        .holds
        .get(engine)
        .filter(|s| !s.owner && s.holds(now()))
    else {
        return;
    };
    if let Some(started) = meter_proves(cfg, engine, s) {
        end_engine(cfg, engine, Some(started), false);
    }
}

/// Read-only check of the hold blocking `run` after discounting any non-owner
/// hold proven over by that engine's fresh meter.
pub fn out_metered_for(cfg: &Config, run: &login::Run) -> Option<Spent> {
    let s = out_for(cfg, run)?;
    if !s.owner && s.engine != ALL_ENGINES && meter_proves(cfg, &s.engine, &s).is_some() {
        return None;
    }
    Some(s)
}

/// Read-only metered hold check on `claude` (or global `*`).
pub fn out_metered(cfg: &Config) -> Option<Spent> {
    out_metered_for(cfg, &login::Run::plan(false))
}

#[cfg(test)]
fn meter_start(cfg: &Config) -> Option<i64> {
    let s = out_for(cfg, &login::Run::plan(false))?;
    (!s.owner && s.engine != ALL_ENGINES)
        .then(|| meter_proves(cfg, &s.engine, &s))
        .flatten()
}

/// When a fresh meter reading for `engine` that proves `s` over was requested.
fn meter_proves(cfg: &Config, engine: &str, s: &Spent) -> Option<i64> {
    use burrow_usage_reset::meter::{FRESH, Snapshot};
    let dir = cfg.meters.as_ref()?;
    let now = now() as f64;
    let meter = Snapshot::read(dir, engine, now)?;
    let requested = meter.requested_at?;
    if !requested.is_finite() || requested > meter.read_at || now - requested >= FRESH {
        return None;
    }
    let started = requested.floor() as i64;
    (started > s.refused() && meter.room()).then_some(started)
}

/// The latest release timestamp for `engine`, retained past hold settlement.
pub fn last_release_for(cfg: &Config, engine: &str) -> Option<i64> {
    let now = now();
    if let Ok(store) = store::read_store(cfg) {
        let kept_rel = [ALL_ENGINES, engine]
            .into_iter()
            .filter_map(|k| store.holds.get(k))
            .map(Spent::release)
            .filter(|&r| r <= now)
            .max();
        if kept_rel.is_some() {
            return kept_rel;
        }
    }
    store::released_for(cfg, engine).ok().filter(|&at| at > 0)
}

/// Convenience wrapper for `claude`'s latest release timestamp.
pub fn last_release(cfg: &Config) -> Option<i64> {
    last_release_for(cfg, DEFAULT_ENGINE)
}

/// Trickle admission nanosecond cutoff for `engine`, serialized with usage updates.
pub fn admission_cutoff_for(cfg: &Config, engine: &str) -> std::io::Result<Option<i64>> {
    let _lock = try_lock(cfg)?;
    store::admission_cutoff_locked(cfg, engine)
}

/// Convenience wrapper for `claude`'s trickle admission cutoff.
pub fn admission_cutoff(cfg: &Config) -> std::io::Result<Option<i64>> {
    admission_cutoff_for(cfg, DEFAULT_ENGINE)
}

/// Under the usage lock, settle any expired owner holds and persist their cutoffs.
fn reconcile_expiry(cfg: &Config) -> Result<(), String> {
    let mut store = store::read_store(cfg)?;
    let now = now();
    let mut dirty = false;
    let mut closed_notices: Vec<(String, String)> = Vec::new();
    for (engine, s) in &mut store.holds {
        if !s.owner || s.back.is_some() || s.until > now {
            continue;
        }
        let cutoff = s.until - 1;
        store::mark_released(cfg, &[engine.as_str()], cutoff)?;
        s.back = Some(s.until);
        s.admission = Some(s.until * 1_000_000_000);
        s.told = now;
        store::write_admission(cfg, engine, s, false)?;
        dirty = true;
        closed_notices.push((notice::key(engine), notice::closed(s)));
    }
    if dirty {
        store::write_store(cfg, &store)?;
        for (engine, s) in &store.holds {
            if s.owner && s.back == Some(s.until) {
                let _ = store::admission_release(cfg, engine, s);
            }
        }
        for (key, msg) in closed_notices {
            notice::tell(cfg, &["close", &key, &msg]);
        }
        changed(cfg);
    }
    Ok(())
}

/// Record a usage refusal on `engine` (or login refusal if `is_login`).
pub fn record_for(
    cfg: &Config,
    engine: &str,
    id: &str,
    until: Option<i64>,
    said: &str,
    at: i64,
) -> Result<Spent, String> {
    record_kind(cfg, engine, id, until, said, login::is(said), at)
}

/// Record an expired-login refusal on `engine`.
pub fn record_login(
    cfg: &Config,
    engine: &str,
    id: &str,
    said: &str,
    at: i64,
) -> Result<Spent, String> {
    record_kind(cfg, engine, id, None, said, true, at)
}

fn record_kind(
    cfg: &Config,
    engine: &str,
    id: &str,
    until: Option<i64>,
    said: &str,
    is_login: bool,
    at: i64,
) -> Result<Spent, String> {
    let _lock = lock(cfg)?;
    let stale_owner_refusal = {
        let store = store::read_store(cfg)?;
        let now = now();
        [ALL_ENGINES, engine].into_iter().any(|k| {
            store.holds.get(k).is_some_and(|s| {
                s.owner
                    && !s.back.is_some_and(|back| back < s.until)
                    && s.until <= now
                    && at < s.until
            })
        })
    };
    reconcile_expiry(cfg)?;
    if stale_owner_refusal {
        let store = store::read_store(cfg)?;
        if let Some(existing) = store
            .holds
            .get(engine)
            .or_else(|| store.holds.get(ALL_ENGINES))
        {
            return Ok(existing.clone());
        }
    }
    put(cfg, engine, id, until, said, is_login, at, at)
}

/// Record a refusal on the default (`claude`) engine.
pub fn record(
    cfg: &Config,
    id: &str,
    until: Option<i64>,
    said: &str,
    at: i64,
) -> Result<Spent, String> {
    record_for(cfg, DEFAULT_ENGINE, id, until, said, at)
}

/// Set an explicit owner pause on `engine` (or `*` for every engine).
pub fn hold_for(cfg: &Config, engine: &str, until: i64, reason: &str) -> Result<(), String> {
    let _lock = lock(cfg)?;
    let at = now();
    if until <= at {
        return Err("proj usage hold: UNTIL must be a future Unix timestamp".into());
    }
    reconcile_expiry(cfg)?;
    let mut store = store::read_store(cfg)?;
    let spent = Spent {
        engine: engine.to_owned(),
        owner: true,
        login: false,
        until,
        said: reason.to_owned(),
        job: "owner".into(),
        at,
        back: None,
        admission: None,
        told: at,
        last: Some(at),
    };
    store.holds.insert(engine.to_owned(), spent.clone());
    store::write_store(cfg, &store)?;
    let key = notice::key(engine);
    notice::tell(
        cfg,
        &[
            "open",
            &key,
            &notice::title(&spent),
            &notice::body(cfg, &spent),
        ],
    );
    changed(cfg);
    Ok(())
}

/// Set an explicit global owner pause over every engine (`*`).
pub fn hold(cfg: &Config, until: i64, reason: &str) -> Result<(), String> {
    hold_for(cfg, ALL_ENGINES, until, reason)
}

#[allow(clippy::too_many_arguments)]
fn put(
    cfg: &Config,
    engine: &str,
    id: &str,
    until: Option<i64>,
    said: &str,
    is_login: bool,
    at: i64,
    last: i64,
) -> Result<Spent, String> {
    let until = until.filter(|&t| t > at).unwrap_or(at + UNSAID);
    let mut store = store::read_store(cfg)?;
    // An active owner pause (global or on this engine) takes precedence.
    for key in [ALL_ENGINES, engine] {
        if let Some(owner) = store.holds.get(key).filter(|w| w.owner && w.holds(at)) {
            return Ok(owner.clone());
        }
    }
    let normalized = store::normalize_legacy_refusal(engine, said);
    let is_login = is_login || login::is(&normalized);
    let was = store
        .holds
        .get(engine)
        .filter(|w| w.back.is_none() && !w.owner)
        .cloned();
    let lapsed = was.as_ref().is_none_or(|w| !w.holds(at));
    let spent = match &was {
        Some(w) => {
            let (said, login_flag) = if lapsed || until > w.until {
                (normalized, is_login)
            } else {
                (w.said.clone(), w.is_login())
            };
            let retold = lapsed || login_flag != w.is_login();
            Spent {
                engine: engine.to_owned(),
                owner: false,
                login: login_flag,
                until: w.until.max(until),
                last: Some(w.refused().max(last)),
                told: if retold { now() } else { w.told },
                said,
                ..w.clone()
            }
        }
        None => Spent {
            engine: engine.to_owned(),
            owner: false,
            login: is_login,
            until,
            said: normalized,
            job: id.to_owned(),
            at,
            back: None,
            admission: None,
            told: now(),
            last: Some(last),
        },
    };
    if was.as_ref() == Some(&spent) {
        return Ok(spent);
    }
    store.holds.insert(engine.to_owned(), spent.clone());
    store::write_store(cfg, &store)?;
    if lapsed
        || was
            .as_ref()
            .is_some_and(|w| w.is_login() != spent.is_login())
    {
        let key = notice::key(engine);
        notice::tell(
            cfg,
            &[
                "open",
                &key,
                &notice::title(&spent),
                &notice::body(cfg, &spent),
            ],
        );
    }
    if lapsed || was.as_ref().is_some_and(|w| w.until != spent.until) {
        changed(cfg);
    }
    Ok(spent)
}

fn changed(cfg: &Config) {
    rewatch_all(cfg);
    let mut kicks = vec!["proj-watch".to_owned()];
    if cfg.waits.is_some() {
        kicks.push("watchdog".to_owned());
    }
    kick(cfg, kicks);
}

/// Re-send open notices for any active holds past `RETELL`.
pub fn remind(cfg: &Config) {
    let now = now();
    let due = |s: &Spent| s.holds(now) && now - s.told >= RETELL;
    let Ok(snapshot) = store::read_store(cfg) else {
        return;
    };
    if !snapshot.holds.values().any(due) {
        return;
    }
    let Ok(_lock) = store::wait_lock(cfg) else {
        return;
    };
    if let Err(e) = reconcile_expiry(cfg) {
        eprintln!("{e}");
        return;
    }
    let Ok(mut store) = store::read_store(cfg) else {
        return;
    };
    let mut reminded = Vec::new();
    for (engine, s) in &mut store.holds {
        if due(s) {
            s.told = now;
            reminded.push((notice::key(engine), s.clone()));
        }
    }
    if !reminded.is_empty() && store::write_store(cfg, &store).is_ok() {
        for (key, s) in reminded {
            notice::tell(
                cfg,
                &["open", &key, &notice::title(&s), &notice::body(cfg, &s)],
            );
        }
    }
}

/// Release holds: when `started` is `None` (`proj usage back`), releases all
/// holds (or only `engine` when `Some`); when `started` is `Some(t)`, releases
/// only `engine`'s non-owner hold if `t > refused`.
pub fn back_for(cfg: &Config, engine: Option<&str>, started: Option<i64>) {
    match (engine, started) {
        (Some(eng), started) => end_engine(cfg, eng, started, false),
        (None, None) => end_all(cfg),
        (None, Some(started)) => end_engine(cfg, DEFAULT_ENGINE, Some(started), false),
    }
}

/// Release all holds (`started = None`) or `claude`'s hold (`started = Some`).
pub fn back(cfg: &Config, started: Option<i64>) {
    back_for(cfg, None, started);
}

/// A run on `engine` started at `started` answered without refusal.
pub fn answered_for(cfg: &Config, engine: &str, started: i64, sandboxed: bool) {
    end_engine(cfg, engine, Some(started), sandboxed);
}

/// A run on `claude` started at `started` answered without refusal.
pub fn answered(cfg: &Config, started: i64, sandboxed: bool) {
    answered_for(cfg, DEFAULT_ENGINE, started, sandboxed);
}

fn end_all(cfg: &Config) {
    let Ok(_lock) = lock(cfg) else { return };
    if let Err(e) = reconcile_expiry(cfg) {
        eprintln!("{e}");
        return;
    }
    let now = now();
    let Ok(mut store) = store::read_store(cfg) else {
        return;
    };
    let mut keys: Vec<String> = store.holds.keys().cloned().collect();
    if !keys.iter().any(|k| k == ALL_ENGINES) {
        keys.push(ALL_ENGINES.to_owned());
    }
    if !keys.iter().any(|k| k == DEFAULT_ENGINE) {
        keys.push(DEFAULT_ENGINE.to_owned());
    }
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    if let Err(e) = store::mark_released(cfg, &key_refs, now) {
        eprintln!("{e}");
        return;
    }
    let mut ended = Vec::new();
    let mut already_ended = Vec::new();
    let cutoff_now = crate::now_nanos();
    for (engine, s) in &mut store.holds {
        if s.back.is_none() {
            s.admission = Some(cutoff_now.min(s.until * 1_000_000_000));
            s.back = Some(now);
            s.told = now;
            if let Err(e) = store::write_admission(cfg, engine, s, false) {
                eprintln!("{e}");
                return;
            }
            ended.push((engine.clone(), s.clone()));
        } else {
            already_ended.push((engine.clone(), s.clone()));
        }
    }
    if !ended.is_empty() {
        if let Err(e) = store::write_store(cfg, &store) {
            eprintln!("{e}");
            return;
        }
        for (engine, s) in &ended {
            let _ = store::admission_release(cfg, engine, s);
            let msg = notice::closed(s);
            eprintln!("proj: {msg} (out since {})", when(s.at));
            notice::tell(cfg, &["close", &notice::key(engine), &msg]);
        }
        changed(cfg);
    } else if !already_ended.is_empty() {
        settle_store(cfg, store, now);
        changed(cfg);
    } else {
        eprintln!("proj: no usage hold is on; no usage limit recorded before now will hold");
        changed(cfg);
    }
}

fn end_engine(cfg: &Config, engine: &str, started: Option<i64>, sandboxed: bool) {
    let owner_action = started.is_none();
    if !owner_action {
        let Ok(snapshot) = store::read_store(cfg) else {
            return;
        };
        if !snapshot.holds.contains_key(engine) {
            return;
        }
    }
    let Ok(_lock) = lock(cfg) else { return };
    if let Err(e) = reconcile_expiry(cfg) {
        eprintln!("{e}");
        return;
    }
    let now = now();
    let Ok(mut store) = store::read_store(cfg) else {
        return;
    };
    let s = store.holds.get(engine).cloned();
    let ends = s.as_ref().is_some_and(|s| s.back.is_none());
    if !owner_action && s.as_ref().is_some_and(|s| s.owner && s.holds(now)) {
        return;
    }
    if ends
        && s.as_ref()
            .is_some_and(|s| !s.holds_run(&login::Run::new(engine, sandboxed)))
    {
        return;
    }
    if ends && started.is_some_and(|t| s.as_ref().is_some_and(|s| t <= s.refused())) {
        return;
    }
    if (owner_action || ends)
        && let Err(e) = store::mark_released(cfg, &[engine], now)
    {
        eprintln!("{e}");
        return;
    }
    match s {
        Some(mut s) if ends => {
            s.admission = Some(crate::now_nanos().min(s.until * 1_000_000_000));
            s.back = Some(now);
            s.told = now;
            if let Err(e) = store::write_admission(cfg, engine, &s, false) {
                eprintln!("{e}");
                return;
            }
            store.holds.insert(engine.to_owned(), s.clone());
            if let Err(e) = store::write_store(cfg, &store) {
                eprintln!("{e}");
                return;
            }
            let _ = store::admission_release(cfg, engine, &s);
            let msg = notice::closed(&s);
            eprintln!("proj: {msg} (out since {})", when(s.at));
            notice::tell(cfg, &["close", &notice::key(engine), &msg]);
            changed(cfg);
        }
        Some(_) => {
            settle_store(cfg, store, now);
            if owner_action {
                changed(cfg);
            }
        }
        None => {
            eprintln!(
                "proj: no usage hold is on for {engine}; no usage limit recorded before now will hold"
            );
            changed(cfg);
        }
    }
}

/// Tend all engine holds and notices on `proj-watch` passes.
pub fn tend(cfg: &Config) -> Option<i64> {
    metered(cfg);
    let now = now();
    let retry = Some(now + RETRY);
    {
        let Ok(_lock) = lock(cfg) else { return retry };
        if let Err(e) = reconcile_expiry(cfg) {
            eprintln!("{e}");
            return retry;
        }
    }
    let store = match store::read_store(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return retry;
        }
    };
    if store.holds.is_empty() {
        return None;
    }
    if store.holds.values().any(|s| s.holds(now)) {
        remind(cfg);
    }
    if store
        .holds
        .values()
        .any(|s| !s.holds(now) && s.back.is_some())
    {
        let Ok(_lock) = lock(cfg) else { return retry };
        if let Ok(fresh) = store::read_store(cfg) {
            settle_store(cfg, fresh, now);
        }
    }
    let fresh = match store::read_store(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return retry;
        }
    };
    let mut earliest: Option<i64> = None;
    for (engine, s) in &fresh.holds {
        let candidate = if s.holds(now) {
            let next = if s.owner {
                (s.told + RETELL).min(s.until)
            } else {
                s.told + RETELL
            };
            let floor = if s.owner { now + 1 } else { now + RETRY };
            Some(next.max(floor))
        } else if s.back.is_some() {
            let next = if notice::pending(cfg, engine) {
                s.told + RETELL / 12
            } else {
                s.release() + cfg.stall_queued()
            };
            Some(next.max(now + RETRY))
        } else {
            None
        };
        if let Some(at) = candidate {
            earliest = Some(earliest.map_or(at, |e| e.min(at)));
        }
    }
    earliest
}

fn settle_store(cfg: &Config, mut store: Store, now: i64) {
    let engines: Vec<String> = store.holds.keys().cloned().collect();
    let mut dirty = false;
    let mut retries = Vec::new();
    for engine in engines {
        let Some(s) = store.holds.get(&engine).cloned() else {
            continue;
        };
        if s.back.is_none() {
            continue;
        }
        if store::admission_release(cfg, &engine, &s).is_err() {
            continue;
        }
        let settled = !notice::pending(cfg, &engine);
        if settled && now >= s.release() + cfg.stall_queued() {
            store.holds.remove(&engine);
            dirty = true;
        } else if !settled && now - s.told >= RETELL / 12 {
            let mut updated = s;
            updated.told = now;
            retries.push((notice::key(&engine), notice::closed(&updated)));
            store.holds.insert(engine, updated);
            dirty = true;
        }
    }
    if dirty && store::write_store(cfg, &store).is_ok() {
        for (key, msg) in retries {
            notice::tell(cfg, &["close", &key, &msg]);
        }
    }
}

/// Adopt an earlier proj's or watchdog's usage limit into `claude`'s hold.
pub fn earlier(cfg: &Config, id: &str, reset: i64, written: Option<i64>) -> Result<(), String> {
    let _lock = lock(cfg)?;
    earlier_locked(cfg, id, reset, written)
}

fn earlier_locked(cfg: &Config, id: &str, reset: i64, written: Option<i64>) -> Result<(), String> {
    reconcile_expiry(cfg)?;
    let store = store::read_store(cfg)?;
    let cutoff = [ALL_ENGINES, DEFAULT_ENGINE]
        .into_iter()
        .filter_map(|k| store.holds.get(k).and_then(Spent::release_cutoff))
        .max()
        .unwrap_or(0);
    let released = store::released_for(cfg, DEFAULT_ENGINE)?.max(cutoff);
    let ended = match written {
        Some(w) => w <= released,
        None => released > 0,
    };
    if reset <= now() || ended {
        return Ok(());
    }
    put(
        cfg,
        DEFAULT_ENGINE,
        id,
        Some(reset),
        EARLIER,
        false,
        now(),
        written.unwrap_or(0),
    )
    .map(|_| ())
}

/// Adopt any legacy `N.resets` files left by an earlier proj generation.
pub fn adopt(cfg: &Config) -> Result<(), String> {
    let _lock = lock(cfg)?;
    reconcile_expiry(cfg)?;
    let dirs = fs::read_dir(&cfg.jobs).map_err(|e| format!("proj: {}: {e}", cfg.jobs.display()))?;
    for e in dirs.filter_map(|e| e.ok()) {
        let id = e.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let Some(n) = job.turns() else { continue };
        let f = job.turn_file(&n, TurnFile::Resets);
        if !f.exists() || !crate::record::outcome_said(&job, &n) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        if let Ok(reset) = text.trim().parse::<i64>() {
            earlier_locked(cfg, &id, reset, mtime(&f))?;
        }
        match fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("proj: {}: {e}", f.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Handle `proj usage [back [--engine E] | hold UNTIL [--engine E] [--only pipeline | --reason TEXT] | adopt UNTIL]`.
pub(crate) fn cmd(cfg: &Config, args: &[String], put: &mut dyn FnMut(&str)) -> Result<(), Fail> {
    match args.get(1).map(String::as_str) {
        None => {
            if let Err(e) = adopt(cfg) {
                eprintln!("{e}");
            }
            let holds = all_out(cfg);
            let mut value = serde_json::Map::new();
            if !holds.is_empty() {
                value.insert("engines".into(), serde_json::json!(holds));
            }
            let meters = crate::github_quota::meters(cfg);
            if !meters.is_empty() {
                value.insert("github".into(), serde_json::json!(meters));
            }
            if !value.is_empty() {
                put(&format!("{}\n", serde_json::Value::Object(value)));
            }
            Ok(())
        }
        Some("back") => match args[2..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] => {
                if let Some(settings) = &cfg.settings {
                    settings
                        .command(&["back".into()], "proj usage back")
                        .map_err(|e| Fail::new(1, e))?;
                }
                back_for(cfg, None, None);
                Ok(())
            }
            ["--engine", engine] if valid_engine_key(engine) => {
                back_for(cfg, Some(engine), None);
                Ok(())
            }
            _ => Err(Fail::new(2, "usage: proj usage back [--engine ENGINE]")),
        },
        Some("hold") => {
            let help =
                "usage: proj usage hold UNTIL [--engine ENGINE] [--only pipeline | --reason TEXT]";
            let until_raw = args.get(2).ok_or_else(|| Fail::new(2, help))?;
            let rest: Vec<&str> = args[3..].iter().map(String::as_str).collect();
            if let ["--only", "pipeline"] = rest.as_slice() {
                let settings = cfg
                    .settings
                    .as_ref()
                    .ok_or_else(|| Fail::new(1, "settings are not configured"))?;
                put(&settings
                    .command(&["pipeline".into(), until_raw.clone()], "proj usage hold")
                    .map_err(|e| Fail::new(1, e))?);
                return Ok(());
            }
            let until = until_raw.parse::<i64>().map_err(|_| Fail::new(2, help))?;
            let (engine, reason) = match rest.as_slice() {
                [] => (ALL_ENGINES, "Work paused by the owner"),
                ["--reason", r] if !r.trim().is_empty() => (ALL_ENGINES, *r),
                ["--engine", e] if valid_engine_key(e) => (*e, "Work paused by the owner"),
                ["--engine", e, "--reason", r] if valid_engine_key(e) && !r.trim().is_empty() => {
                    (*e, *r)
                }
                ["--reason", r, "--engine", e] if valid_engine_key(e) && !r.trim().is_empty() => {
                    (*e, *r)
                }
                _ => return Err(Fail::new(2, help)),
            };
            hold_for(cfg, engine, until, reason).map_err(|e| Fail::new(1, e))
        }
        Some("adopt") => {
            if args.len() != 3 {
                return Err(Fail::new(2, "usage: proj usage adopt UNTIL"));
            }
            let until = args[2]
                .parse::<i64>()
                .map_err(|_| Fail::new(2, "usage: proj usage adopt UNTIL (seconds)"))?;
            earlier(cfg, "the stall watchdog", until, None).map_err(|e| Fail::new(1, e))
        }
        Some(_) => Err(Fail::new(
            2,
            "usage: proj usage [back [--engine ENGINE] | hold UNTIL [--engine ENGINE] [--reason TEXT] | adopt UNTIL]",
        )),
    }
}

fn valid_engine_key(s: &str) -> bool {
    !s.is_empty()
        && s != ALL_ENGINES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
use store::{CONTENTION, RELEASED, wait_lock};

#[cfg(test)]
fn keep(cfg: &Config, s: &Spent) -> Result<(), String> {
    let key = if s.engine.is_empty() {
        if s.owner { ALL_ENGINES } else { DEFAULT_ENGINE }
    } else {
        s.engine.as_str()
    };
    let mut store = store::read_store(cfg)?;
    let mut entry = s.clone();
    entry.engine = key.to_owned();
    store.holds.insert(key.to_owned(), entry);
    store::write_store(cfg, &store)
}

#[cfg(test)]
fn released(cfg: &Config) -> Result<i64, String> {
    store::released_for(cfg, DEFAULT_ENGINE)
}

#[cfg(test)]
fn mark(cfg: &Config, at: i64) -> Result<(), String> {
    store::mark_released(cfg, &[DEFAULT_ENGINE], at)
}

#[cfg(test)]
fn settle(cfg: &Config, s: Spent, at: i64) {
    let key = if s.engine.is_empty() {
        if s.owner { ALL_ENGINES } else { DEFAULT_ENGINE }
    } else {
        s.engine.as_str()
    };
    let mut store = store::read_store(cfg).unwrap_or_default();
    store.holds.insert(key.to_owned(), s);
    settle_store(cfg, store, at);
}

#[cfg(test)]
fn body(cfg: &Config, s: &Spent) -> String {
    notice::body(cfg, s)
}

#[cfg(test)]
fn admission_cutoff_locked(cfg: &Config) -> std::io::Result<Option<i64>> {
    store::admission_cutoff_locked(cfg, DEFAULT_ENGINE)
}

#[cfg(test)]
fn admission_release(cfg: &Config, s: &Spent) -> Result<i64, String> {
    let key = if s.engine.is_empty() {
        if s.owner { ALL_ENGINES } else { DEFAULT_ENGINE }
    } else {
        s.engine.as_str()
    };
    store::admission_release(cfg, key, s)
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod owner_tests;
