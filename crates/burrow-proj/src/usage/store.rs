//! On-disk persistence and locking for per-engine usage holds (`jobs/.usage-holds.json`),
//! release watermarks (`jobs/.usage-holds.released`), and trickle admission cutoffs
//! (`jobs/.usage-holds.admission`).
//!
//! Writes keep the authoritative v2 per-engine store in `.usage-holds.*` and mirror
//! any `claude` or global `*` state, or an active `claude-api` owner pause, into the
//! legacy `.usage.json` / `.usage.released` / `.usage.admission` files in strict
//! pre-change formats under the shared `jobs/.usage.lock`. Retained pre-change `proj`
//! turns running across a deploy therefore keep parsing `.usage.json` cleanly, honor
//! newly set Claude/API/global holds, cannot clear active owner pauses early, and persist any later refusal so the new binary merges it on next read.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::{now, swap};

use super::login;

/// Authoritative v2 per-engine usage hold store, in `jobs`.
pub(crate) const FILE: &str = ".usage-holds.json";
/// Legacy single-hold file mirrored in strict pre-change format for retained deploy turns.
pub(crate) const LEGACY_FILE: &str = ".usage.json";
/// Per-engine last release timestamps (seconds), kept past hold settlement so
/// stale pre-release imports never re-hold an engine.
pub(crate) const RELEASED: &str = ".usage-holds.released";
/// Legacy scalar release watermark mirrored for retained pre-change turns.
pub(crate) const LEGACY_RELEASED: &str = ".usage.released";
/// Per-engine trickle admission cutoffs (nanoseconds), kept past hold settlement.
pub(crate) const ADMISSION: &str = ".usage-holds.admission";
/// Legacy tab-separated admission cutoff mirrored for retained pre-change turns.
pub(crate) const LEGACY_ADMISSION: &str = ".usage.admission";
/// Key representing an owner pause over every engine.
pub const ALL_ENGINES: &str = "*";
/// Default engine key used when reading legacy single-hold files.
pub const DEFAULT_ENGINE: &str = "claude";
/// Current on-disk format version for `.usage.json`.
pub const VERSION: u32 = 2;

const LEGACY_OWNER: &str = ".usage-owner-";
const LEGACY_ADMISSION_REF: &str = ".usage-release-";
pub(crate) const CONTENTION: Duration = Duration::from_secs(1);

/// One engine's hold (or a global owner pause when `engine == ALL_ENGINES`).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    /// Engine key this record belongs to (`*` for an owner pause over all engines).
    #[serde(skip)]
    pub engine: String,
    /// An explicit owner pause: only the owner or its deadline releases it.
    #[serde(default)]
    pub owner: bool,
    /// True when the refusal was an expired host login rather than a usage quota.
    #[serde(default)]
    pub login: bool,
    pub until: i64,
    pub said: String,
    pub job: String,
    pub at: i64,
    /// When the hold ended early (by a successful run, fresh meter, or `proj usage back`).
    #[serde(default)]
    pub back: Option<i64>,
    /// Precise nanosecond admission cutoff committed atomically with `back`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<i64>,
    /// When the owner's notice was last sent (open while held, close after).
    #[serde(default)]
    pub told: i64,
    /// When a run was last refused while this hold stood.
    #[serde(default)]
    pub last: Option<i64>,
}

impl Spent {
    /// When the line moved again: its reset, or sooner if it ended sooner.
    pub fn release(&self) -> i64 {
        self.back.map_or(self.until, |b| b.min(self.until))
    }

    pub(crate) fn admission_cutoff(&self) -> i64 {
        if self.back.is_some() {
            self.admission.unwrap_or(self.release() * 1_000_000_000)
        } else {
            self.until * 1_000_000_000
        }
    }

    /// Whether this hold blocks new runs at `now`.
    pub fn holds(&self, now: i64) -> bool {
        self.back.is_none() && self.until > now
    }

    /// Whether this hold represents an expired host login.
    pub fn is_login(&self) -> bool {
        !self.owner && (self.login || login::is(&self.said))
    }

    /// Expiry suppresses only pre-deadline imports; explicit releases are inclusive.
    pub(crate) fn release_cutoff(&self) -> Option<i64> {
        self.back.map(|back| {
            if self.owner && back == self.until {
                back - 1
            } else {
                back
            }
        })
    }

    /// When a run was last refused: a run started then or before proves nothing now.
    pub fn refused(&self) -> i64 {
        self.last.unwrap_or(self.at)
    }
}

/// On-disk v2 per-engine usage hold store (`jobs/.usage.json`).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Store {
    pub version: u32,
    #[serde(default)]
    pub holds: BTreeMap<String, Spent>,
}

impl Default for Store {
    fn default() -> Self {
        Self::empty()
    }
}

impl Store {
    pub fn empty() -> Self {
        Self {
            version: VERSION,
            holds: BTreeMap::new(),
        }
    }

    /// Whether any active hold in this store blocks `run` at `at`.
    pub fn holds_run(&self, run: &login::Run, at: i64) -> bool {
        self.hold_for(run, at).is_some()
    }

    /// The active hold blocking `run` at `at`, if any. Global owner pauses (`*`)
    /// take precedence over engine-specific refusal holds.
    pub fn hold_for(&self, run: &login::Run, at: i64) -> Option<&Spent> {
        if let Some(global) = self.holds.get(ALL_ENGINES).filter(|s| s.holds(at)) {
            return Some(global);
        }
        self.holds
            .get(&run.engine)
            .filter(|s| s.holds(at) && s.holds_run(run))
    }

    /// Any kept record (holding or recently released) that applies to `run`.
    pub fn kept_for(&self, run: &login::Run) -> Option<&Spent> {
        let now = now();
        if let Some(global) = self.holds.get(ALL_ENGINES).filter(|s| s.holds(now)) {
            return Some(global);
        }
        if let Some(eng) = self.holds.get(&run.engine).filter(|s| s.holds_run(run)) {
            return Some(eng);
        }
        self.holds.get(ALL_ENGINES).filter(|s| s.holds_run(run))
    }
}

/// Strict pre-change single-hold `.usage.json` schema parsed by retained deploy turns.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySpent {
    until: i64,
    said: String,
    job: String,
    at: i64,
    #[serde(default)]
    back: Option<i64>,
    #[serde(default, skip_serializing)]
    admission: Option<i64>,
    #[serde(default)]
    told: i64,
    #[serde(default)]
    last: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Admission {
    pub until: i64,
    pub cutoff: i64,
    pub committed: bool,
}

fn legacy_owner_ref(job: &str) -> bool {
    job.strip_prefix(LEGACY_OWNER).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn legacy_owner_metadata(cfg: &Config, job: &str) -> Result<Option<Spent>, String> {
    if !legacy_owner_ref(job) {
        return Ok(None);
    }
    let bytes = fs::read(cfg.jobs.join(job)).map_err(|e| format!("proj: {job}: {e}"))?;
    let legacy: LegacySpent =
        serde_json::from_slice(&bytes).map_err(|e| format!("proj: {job}: {e}"))?;
    Ok(Some(Spent {
        engine: ALL_ENGINES.to_owned(),
        owner: true,
        login: false,
        until: legacy.until,
        said: legacy.said,
        job: legacy.job,
        at: legacy.at,
        back: legacy.back,
        admission: legacy.admission,
        told: legacy.told,
        last: legacy.last,
    }))
}

/// Sanitize raw billing/settings URLs from pre-change `.usage.json` records
/// during deploy overlap; plugins normalize new refusal reasons themselves.
pub(super) fn normalize_legacy_refusal(engine: &str, said: &str) -> String {
    if engine != DEFAULT_ENGINE {
        return said.to_owned();
    }
    let s = said.to_lowercase();
    if s.contains("https://") || s.contains("monthly spend limit") || s.contains("raise it at") {
        let name = if s.contains("weekly") {
            "weekly allowance"
        } else if s.contains("5-hour") || s.contains("five-hour") {
            "five-hour allowance"
        } else if s.contains("extra usage") || s.contains("spend limit") {
            "extra-usage cap"
        } else {
            "usage allowance"
        };
        return format!("Claude reached its {name}");
    }
    said.to_owned()
}

fn normalize_loaded(engine: &str, s: &mut Spent) {
    s.engine = engine.to_owned();
    if !s.owner {
        s.said = normalize_legacy_refusal(engine, &s.said);
    }
    if !s.owner && login::is(&s.said) {
        s.login = true;
    }
}

/// The one engine besides `claude` that pre-change `proj` runs. Its runs bill the
/// API credit, so a pre-change reader holds them only for an owner pause
/// (`owner = true`), never for a refusal hold.
const LEGACY_API_ENGINE: &str = "claude-api";

/// Deterministic `.usage-owner-<hex>` metadata filename for the owner pause the
/// legacy mirror fences retained runs with, so retained pre-change `proj` readers
/// recognize `owner = true` (blocking `claude-api` runs as well as `claude` runs)
/// and `project_legacy` remains pure and time-independent. The engine key is part
/// of the name, so reading it back attributes the mirror to the hold it came from.
fn legacy_owner_mirror_name(engine: &str, s: &Spent) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in engine
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(s.until.to_le_bytes())
        .chain(s.at.to_le_bytes())
        .chain(s.said.as_bytes().iter().copied())
    {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{LEGACY_OWNER}{hash:016x}")
}

/// The hold the legacy mirror projects (its engine key and record), and whether it
/// is projected as an owner mirror. The legacy file carries one record, so, first
/// match wins:
/// 1. an active global `*` hold (an owner mirror when it is an owner pause);
/// 2. an active `claude-api` owner pause, as an owner mirror: a pre-change reader
///    holds `claude-api` runs only for `owner = true`, which holds its `claude`
///    runs too, so retained turns stay held on both engines while it stands (fail
///    closed; a `claude` hold outlasting it is projected again on the next write);
/// 3. the `claude` hold, plain, so a `claude`-only owner pause leaves retained
///    `claude-api` runs free;
/// 4. a settled global `*` record.
fn legacy_source(store: &Store) -> Option<(&str, &Spent, bool)> {
    let active_owner = |s: &&Spent| s.owner && s.back.is_none();
    if let Some(global) = store.holds.get(ALL_ENGINES).filter(|s| s.back.is_none()) {
        return Some((ALL_ENGINES, global, global.owner));
    }
    if let Some(api) = store.holds.get(LEGACY_API_ENGINE).filter(active_owner) {
        return Some((LEGACY_API_ENGINE, api, true));
    }
    if let Some(claude) = store.holds.get(DEFAULT_ENGINE) {
        return Some((DEFAULT_ENGINE, claude, false));
    }
    store
        .holds
        .get(ALL_ENGINES)
        .map(|s| (ALL_ENGINES, s, false))
}

/// Pure, time-independent projection of `store`'s hold ([`legacy_source`]) into
/// the strict 7-field `LegacySpent` format read by retained pre-change `proj` turns.
/// Active owner pauses set `last = until - 1` so an early successful old turn
/// cannot clear them while a post-deadline refusal at `>= until` still registers.
/// An owner mirror points `job` at a `.usage-owner-<hex>` metadata file so retained
/// pre-change readers set `owner = true` and hold `claude-api` turns too; a
/// `claude`-only owner pause keeps `job` plain so retained `claude-api` turns stay
/// free.
fn project_legacy(store: &Store) -> Option<LegacySpent> {
    let (engine, pick, mirror) = legacy_source(store)?;
    let job = if mirror {
        legacy_owner_mirror_name(engine, pick)
    } else {
        pick.job.clone()
    };
    let last = if pick.owner && pick.back.is_none() {
        Some(pick.until.saturating_sub(1).max(pick.at))
    } else {
        pick.last
    };
    Some(LegacySpent {
        until: pick.until,
        said: pick.said.clone(),
        job,
        at: pick.at,
        back: pick.back,
        admission: None,
        told: pick.told,
        last,
    })
}

/// The engine whose owner pause `store`'s legacy mirror currently fences retained
/// runs with, other than the global `*`, and that mirror's metadata name.
fn legacy_mirror_of(store: &Store) -> Option<(String, String)> {
    let (engine, pick, mirror) = legacy_source(store)?;
    (mirror && engine != ALL_ENGINES)
        .then(|| (engine.to_owned(), legacy_owner_mirror_name(engine, pick)))
}

fn decode_legacy(cfg: &Config, legacy: LegacySpent) -> Result<(String, Spent), String> {
    let mut s = Spent {
        engine: DEFAULT_ENGINE.to_owned(),
        owner: false,
        login: false,
        until: legacy.until,
        said: legacy.said,
        job: legacy.job,
        at: legacy.at,
        back: legacy.back,
        admission: legacy.admission,
        told: legacy.told,
        last: legacy.last,
    };
    if s.back.is_some()
        && let Some((cutoff, job)) = s
            .job
            .strip_prefix(LEGACY_ADMISSION_REF)
            .and_then(|reference| reference.split_once('-'))
        && let Ok(cutoff) = cutoff.parse::<i64>()
    {
        s.admission = Some(cutoff);
        s.job = job.to_owned();
    }
    if let Some(back) = s.back
        && s.admission.is_none()
    {
        s.admission = Some(((back + 1) * 1_000_000_000).min(s.until * 1_000_000_000));
    }
    if let Some(mut owner) = legacy_owner_metadata(cfg, &s.job)? {
        if s.refused() >= owner.until {
            s.said = super::EARLIER.into();
            normalize_loaded(DEFAULT_ENGINE, &mut s);
            return Ok((DEFAULT_ENGINE.to_owned(), s));
        }
        owner.owner = true;
        owner.engine = ALL_ENGINES.to_owned();
        owner.told = s.told;
        owner.back = s.back;
        owner.admission = s.admission;
        return Ok((ALL_ENGINES.to_owned(), owner));
    }
    normalize_loaded(DEFAULT_ENGINE, &mut s);
    Ok((DEFAULT_ENGINE.to_owned(), s))
}

/// Merge an update written to legacy `.usage.json` by a retained pre-change `proj`
/// turn into the authoritative v2 `store` without losing other engines' holds or
/// weakening an active owner pause.
fn merge_legacy(store: &mut Store, legacy_engine: String, legacy: Spent) {
    if legacy_engine == ALL_ENGINES {
        match store.holds.get(ALL_ENGINES) {
            None => {
                store.holds.insert(ALL_ENGINES.to_owned(), legacy);
            }
            Some(cur) if legacy.until > cur.until || legacy.at > cur.at => {
                store.holds.insert(ALL_ENGINES.to_owned(), legacy);
            }
            _ => {}
        }
        return;
    }

    // The owner mirror of an engine's own pause (`claude-api`'s): a retained old
    // turn only rewrites it to note its notice or settle its deadline, neither of
    // which may clear, replace or widen the pause. Its deadline ends it here.
    if legacy_engine != DEFAULT_ENGINE {
        if let Some(cur) = store.holds.get_mut(&legacy_engine) {
            cur.told = cur.told.max(legacy.told);
        }
        return;
    }

    // Active global `*` owner pause: retained old turns before its deadline cannot
    // clear or replace it; only a genuine post-deadline refusal (`refused >= until`)
    // creates a `claude` refusal hold.
    if let Some(global) = store.holds.get_mut(ALL_ENGINES)
        && global.owner
        && global.back.is_none()
        && legacy.refused() < global.until
    {
        global.told = global.told.max(legacy.told);
        return;
    }

    match store.holds.get_mut(DEFAULT_ENGINE) {
        None => {
            store.holds.insert(DEFAULT_ENGINE.to_owned(), legacy);
        }
        Some(cur) if cur.owner && cur.back.is_none() => {
            if legacy.refused() >= cur.until && legacy.back.is_none() {
                *cur = legacy;
            } else {
                cur.told = cur.told.max(legacy.told);
            }
        }
        Some(cur) if cur.back.is_none() => {
            if legacy.back.is_some() {
                if legacy.at == cur.at || legacy.back.is_some_and(|b| b >= cur.refused()) {
                    cur.back = legacy.back;
                    cur.admission = legacy.admission;
                    cur.told = cur.told.max(legacy.told);
                }
            } else if legacy.until > cur.until || legacy.refused() > cur.refused() {
                cur.until = cur.until.max(legacy.until);
                cur.last = Some(cur.refused().max(legacy.refused()));
                cur.said = legacy.said;
                cur.job = legacy.job;
                cur.login = legacy.login;
                cur.told = cur.told.max(legacy.told);
            } else {
                cur.told = cur.told.max(legacy.told);
            }
        }
        Some(cur) => {
            // `cur` is already released (`cur.back.is_some()`). Accept a fresh active
            // refusal written by a retained pre-change binary after release.
            if legacy.back.is_none()
                && (legacy.at >= cur.back.unwrap_or(0)
                    || legacy.refused() > cur.release_cutoff().unwrap_or(0)
                    || legacy.until != cur.until)
            {
                *cur = legacy;
            } else {
                cur.told = cur.told.max(legacy.told);
            }
        }
    }
}

/// Read the per-engine hold store from `jobs/.usage-holds.json`, merging any
/// legacy single-hold `.usage.json` state written before deploy or by retained
/// pre-change `proj` turns.
pub(crate) fn read_store(cfg: &Config) -> Result<Store, String> {
    let v2_path = cfg.jobs.join(FILE);
    let mut store = match fs::read_to_string(&v2_path) {
        Ok(text) => {
            let mut s: Store = serde_json::from_str(&text)
                .map_err(|e| format!("proj: {}: {e}", v2_path.display()))?;
            if s.version != VERSION {
                return Err(format!(
                    "proj: {}: unsupported usage store version {} (expected {VERSION})",
                    v2_path.display(),
                    s.version
                ));
            }
            for (engine, spent) in &mut s.holds {
                normalize_loaded(engine, spent);
            }
            Some(s)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("proj: {}: {e}", v2_path.display())),
    };

    let legacy_path = cfg.jobs.join(LEGACY_FILE);
    let legacy_text = match fs::read_to_string(&legacy_path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("proj: {}: {e}", legacy_path.display())),
    };

    let Some(legacy_text) = legacy_text else {
        return Ok(store.unwrap_or_default());
    };

    let legacy_raw: LegacySpent = serde_json::from_str(&legacy_text)
        .map_err(|e| format!("proj: {}: {e}", legacy_path.display()))?;

    if let Some(ref s) = store
        && project_legacy(s).as_ref() == Some(&legacy_raw)
    {
        return Ok(s.clone());
    }

    let mirror = store.as_ref().and_then(legacy_mirror_of);
    let (mut legacy_engine, mut legacy_spent) = decode_legacy(cfg, legacy_raw)?;
    // An owner mirror decodes as a global pause; one of an engine's own pause is
    // that engine's, never a new `*` hold over every engine.
    if legacy_engine == ALL_ENGINES
        && let Some((engine, name)) = mirror
        && legacy_spent.job == name
    {
        legacy_spent.engine.clone_from(&engine);
        legacy_engine = engine;
    }
    let Some(ref mut s) = store else {
        let mut s = Store::empty();
        s.holds.insert(legacy_engine, legacy_spent);
        return Ok(s);
    };
    merge_legacy(s, legacy_engine, legacy_spent);
    Ok(store.unwrap())
}

fn remove_if_present(path: &std::path::Path, label: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("proj: {label}: {e}")),
    }
}

fn cleanup_legacy_owner_mirrors(cfg: &Config) {
    let Ok(entries) = fs::read_dir(&cfg.jobs) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if legacy_owner_ref(&name) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Write the per-engine store atomically to `jobs/.usage-holds.json` (or remove it
/// when empty), then mirror any `claude` or `*` state to `jobs/.usage.json` in
/// strict pre-change `LegacySpent` format for retained deploy turns.
pub(crate) fn write_store(cfg: &Config, store: &Store) -> Result<(), String> {
    let path = cfg.jobs.join(FILE);
    if store.holds.is_empty() {
        remove_if_present(&path, FILE)?;
    } else {
        let text = serde_json::to_string(&Store {
            version: VERSION,
            holds: store.holds.clone(),
        })
        .map_err(|e| format!("proj: {FILE}: {e}"))?;
        swap(&path, &format!("{text}\n")).map_err(|e| format!("proj: {FILE}: {e}"))?;
    }

    let legacy_path = cfg.jobs.join(LEGACY_FILE);
    match project_legacy(store) {
        None => {
            remove_if_present(&legacy_path, LEGACY_FILE)?;
            if store.holds.is_empty() {
                cleanup_legacy_owner_mirrors(cfg);
            }
            Ok(())
        }
        Some(legacy) => {
            let text =
                serde_json::to_string(&legacy).map_err(|e| format!("proj: {LEGACY_FILE}: {e}"))?;
            if legacy_owner_ref(&legacy.job) {
                let meta_path = cfg.jobs.join(&legacy.job);
                swap(&meta_path, &format!("{text}\n"))
                    .map_err(|e| format!("proj: {}: {e}", legacy.job))?;
            }
            swap(&legacy_path, &format!("{text}\n"))
                .map_err(|e| format!("proj: {LEGACY_FILE}: {e}"))
        }
    }
}

/// Per-engine last release timestamps (`jobs/.usage-holds.released`), merging
/// legacy scalar `jobs/.usage.released` timestamps too.
pub(crate) fn released_map(cfg: &Config) -> Result<BTreeMap<String, i64>, String> {
    let mut map = BTreeMap::new();
    let v2_path = cfg.jobs.join(RELEASED);
    match fs::read_to_string(&v2_path) {
        Ok(text) => {
            let parsed: BTreeMap<String, i64> = serde_json::from_str(text.trim())
                .map_err(|e| format!("proj: {}: {e}", v2_path.display()))?;
            map.extend(parsed);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("proj: {}: {e}", v2_path.display())),
    }

    let legacy_path = cfg.jobs.join(LEGACY_RELEASED);
    match fs::read_to_string(&legacy_path) {
        Ok(text) => {
            let trimmed = text.trim();
            let ts = trimmed
                .parse::<i64>()
                .map_err(|e| format!("proj: {}: {e}", legacy_path.display()))?;
            let entry = map.entry(DEFAULT_ENGINE.to_owned()).or_insert(0);
            *entry = (*entry).max(ts);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("proj: {}: {e}", legacy_path.display())),
    }
    Ok(map)
}

/// Latest release timestamp for `engine` (accounting for global `*` releases too).
pub(crate) fn released_for(cfg: &Config, engine: &str) -> Result<i64, String> {
    let map = released_map(cfg)?;
    let eng = map.get(engine).copied().unwrap_or(0);
    let global = map.get(ALL_ENGINES).copied().unwrap_or(0);
    Ok(eng.max(global))
}

/// Record release timestamp `at` for every key in `keys`, writing the v2 map and
/// keeping legacy `jobs/.usage.released` as a plain integer for retained deploy turns.
pub(crate) fn mark_released(cfg: &Config, keys: &[&str], at: i64) -> Result<(), String> {
    let mut map = released_map(cfg)?;
    for &key in keys {
        let entry = map.entry(key.to_owned()).or_insert(0);
        *entry = (*entry).max(at);
    }
    let text = serde_json::to_string(&map).map_err(|e| format!("proj: {RELEASED}: {e}"))?;
    swap(&cfg.jobs.join(RELEASED), &format!("{text}\n"))
        .map_err(|e| format!("proj: {RELEASED}: {e}"))?;
    if keys.contains(&DEFAULT_ENGINE) || keys.contains(&ALL_ENGINES) {
        let claude = map.get(DEFAULT_ENGINE).copied().unwrap_or(0);
        let global = map.get(ALL_ENGINES).copied().unwrap_or(0);
        swap(
            &cfg.jobs.join(LEGACY_RELEASED),
            &format!("{}\n", claude.max(global)),
        )
        .map_err(|e| format!("proj: {LEGACY_RELEASED}: {e}"))?;
    }
    Ok(())
}

fn read_admissions(cfg: &Config) -> BTreeMap<String, Admission> {
    let mut map: BTreeMap<String, Admission> = crate::read(&cfg.jobs.join(ADMISSION))
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Admission>>(text.trim()).ok())
        .unwrap_or_default();

    // Merge legacy single-line `<json-or-key>\t<cutoff>` format (`claude`).
    if let Some(text) = crate::read(&cfg.jobs.join(LEGACY_ADMISSION))
        && let Some((key, cutoff)) = text.trim().split_once('\t')
    {
        let legacy = serde_json::from_str::<Admission>(key).ok().or_else(|| {
            cutoff.trim().parse::<i64>().ok().map(|c| Admission {
                until: c / 1_000_000_000,
                cutoff: c,
                committed: true,
            })
        });
        if let Some(a) = legacy {
            let covered_by_v2 = map.get(ALL_ENGINES).is_some_and(|g| g.until >= a.until)
                || map.get(DEFAULT_ENGINE).is_some_and(|c| c.until >= a.until);
            if !covered_by_v2 {
                map.insert(DEFAULT_ENGINE.to_owned(), a);
            }
        }
    }
    map
}

pub(crate) fn write_admission(
    cfg: &Config,
    engine: &str,
    s: &Spent,
    committed: bool,
) -> Result<i64, String> {
    let cutoff = s.admission_cutoff();
    let entry = Admission {
        until: s.until,
        cutoff,
        committed,
    };
    let mut map = read_admissions(cfg);
    map.insert(engine.to_owned(), entry.clone());
    let text = serde_json::to_string(&map).map_err(|e| e.to_string())?;
    swap(&cfg.jobs.join(ADMISSION), &format!("{text}\n")).map_err(|e| e.to_string())?;
    if engine == DEFAULT_ENGINE || engine == ALL_ENGINES {
        let legacy_key = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        swap(
            &cfg.jobs.join(LEGACY_ADMISSION),
            &format!("{legacy_key}\t{cutoff}\n"),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(cutoff)
}

pub(crate) fn admission_release(cfg: &Config, engine: &str, s: &Spent) -> Result<i64, String> {
    write_admission(cfg, engine, s, true)
}

fn effective_admission(a: &Admission) -> i64 {
    if a.committed {
        a.cutoff
    } else {
        a.until * 1_000_000_000
    }
}

pub(crate) fn admission_cutoff_locked(cfg: &Config, engine: &str) -> io::Result<Option<i64>> {
    let store = read_store(cfg).map_err(io::Error::other)?;
    let now = now();
    let mut best: Option<i64> = None;
    let admissions = read_admissions(cfg);
    for key in [ALL_ENGINES, engine] {
        if let Some(s) = store.holds.get(key) {
            if s.holds(now) {
                return Ok(None);
            }
            let c = s.admission_cutoff();
            best = Some(best.map_or(c, |b| b.max(c)));
        } else if let Some(a) = admissions.get(key) {
            let c = effective_admission(a);
            best = Some(best.map_or(c, |b| b.max(c)));
        }
    }
    Ok(best)
}

/// `jobs/.usage.lock`, held: every change to the record goes under it.
pub(crate) fn lock(cfg: &Config) -> Result<crate::held::Held, String> {
    let path = cfg.jobs.join(".usage.lock");
    let f = File::create(&path).map_err(|e| format!("proj: {}: {e}", path.display()))?;
    crate::held::Held::lock(f).map_err(|e| format!("proj: {}: {e}", path.display()))
}

/// Non-blocking lock attempt on `jobs/.usage.lock` for callers holding another lock.
pub(crate) fn try_lock(cfg: &Config) -> io::Result<File> {
    let f = File::create(cfg.jobs.join(".usage.lock"))?;
    f.try_lock().map_err(io::Error::from)?;
    Ok(f)
}

/// Bounded wait on `jobs/.usage.lock` for callers holding no other lock.
pub(crate) fn wait_lock(cfg: &Config) -> io::Result<File> {
    let deadline = Instant::now() + CONTENTION;
    loop {
        match try_lock(cfg) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            got => return got,
        }
    }
}
