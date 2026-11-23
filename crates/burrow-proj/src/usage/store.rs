//! On-disk persistence and locking for per-engine usage holds (`jobs/.usage.json`),
//! release watermarks (`jobs/.usage.released`), and trickle admission cutoffs
//! (`jobs/.usage.admission`).
//!
//! Writes use only the v2 per-engine JSON schema; reads also accept the legacy
//! single-hold `.usage.json`, `.usage-owner-*`, `ADMISSION_REF`, scalar
//! `.usage.released`, and tab-separated `.usage.admission` formats during cutover.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::{now, swap};

use super::login;

/// Where per-engine usage holds are kept, in `jobs`.
pub(crate) const FILE: &str = ".usage.json";
/// Per-engine last release timestamps (seconds), kept past hold settlement so
/// stale pre-release imports never re-hold an engine.
pub(crate) const RELEASED: &str = ".usage.released";
/// Per-engine trickle admission cutoffs (nanoseconds), kept past hold settlement.
pub(crate) const ADMISSION: &str = ".usage.admission";
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

/// Legacy single-hold `.usage.json` record (read-only compatibility during cutover).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySpent {
    until: i64,
    said: String,
    job: String,
    at: i64,
    #[serde(default)]
    back: Option<i64>,
    #[serde(default)]
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

fn normalize_loaded(engine: &str, s: &mut Spent) {
    s.engine = engine.to_owned();
    if !s.owner && engine == DEFAULT_ENGINE {
        s.said = burrow_engine_anthropic::claude::normalize_refusal(&s.said);
    }
    if !s.owner && login::is(&s.said) {
        s.login = true;
    }
}

/// Read the per-engine hold store from `jobs/.usage.json` (accepting legacy single-hold files).
pub(crate) fn read_store(cfg: &Config) -> Result<Store, String> {
    let path = cfg.jobs.join(FILE);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Store::empty()),
        Err(e) => return Err(format!("proj: {}: {e}", path.display())),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("proj: {}: {e}", path.display()))?;
    if value.get("holds").is_some() || value.get("version").is_some() {
        let mut store: Store =
            serde_json::from_value(value).map_err(|e| format!("proj: {}: {e}", path.display()))?;
        if store.version != VERSION {
            return Err(format!(
                "proj: {}: unsupported usage store version {} (expected {VERSION})",
                path.display(),
                store.version
            ));
        }
        for (engine, spent) in &mut store.holds {
            normalize_loaded(engine, spent);
        }
        return Ok(store);
    }
    let legacy: LegacySpent =
        serde_json::from_value(value).map_err(|e| format!("proj: {}: {e}", path.display()))?;
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
    let mut store = Store::empty();
    if let Some(mut owner) = legacy_owner_metadata(cfg, &s.job)? {
        if s.refused() >= owner.until {
            s.said = super::EARLIER.into();
            normalize_loaded(DEFAULT_ENGINE, &mut s);
            store.holds.insert(DEFAULT_ENGINE.to_owned(), s);
        } else {
            owner.owner = true;
            owner.engine = ALL_ENGINES.to_owned();
            owner.told = s.told;
            owner.back = s.back;
            owner.admission = s.admission;
            store.holds.insert(ALL_ENGINES.to_owned(), owner);
        }
        return Ok(store);
    }
    normalize_loaded(DEFAULT_ENGINE, &mut s);
    store.holds.insert(DEFAULT_ENGINE.to_owned(), s);
    Ok(store)
}

/// Write the per-engine store in v2 format atomically, or remove `.usage.json` when empty.
pub(crate) fn write_store(cfg: &Config, store: &Store) -> Result<(), String> {
    let path = cfg.jobs.join(FILE);
    if store.holds.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("proj: {FILE}: {e}")),
        }
    }
    let text = serde_json::to_string(&Store {
        version: VERSION,
        holds: store.holds.clone(),
    })
    .map_err(|e| format!("proj: {FILE}: {e}"))?;
    swap(&path, &format!("{text}\n")).map_err(|e| format!("proj: {FILE}: {e}"))
}

/// Per-engine last release timestamps (`jobs/.usage.released`), reading legacy scalar files too.
pub(crate) fn released_map(cfg: &Config) -> Result<BTreeMap<String, i64>, String> {
    let path = cfg.jobs.join(RELEASED);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("proj: {}: {e}", path.display())),
    };
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        serde_json::from_str::<BTreeMap<String, i64>>(trimmed)
            .map_err(|e| format!("proj: {}: {e}", path.display()))
    } else {
        let ts = trimmed
            .parse::<i64>()
            .map_err(|e| format!("proj: {}: {e}", path.display()))?;
        let mut map = BTreeMap::new();
        map.insert(DEFAULT_ENGINE.to_owned(), ts);
        Ok(map)
    }
}

/// Latest release timestamp for `engine` (accounting for global `*` releases too).
pub(crate) fn released_for(cfg: &Config, engine: &str) -> Result<i64, String> {
    let map = released_map(cfg)?;
    let eng = map.get(engine).copied().unwrap_or(0);
    let global = map.get(ALL_ENGINES).copied().unwrap_or(0);
    Ok(eng.max(global))
}

/// Record release timestamp `at` for every key in `keys`, writing the v2 JSON map format.
pub(crate) fn mark_released(cfg: &Config, keys: &[&str], at: i64) -> Result<(), String> {
    let mut map = released_map(cfg)?;
    for &key in keys {
        let entry = map.entry(key.to_owned()).or_insert(0);
        *entry = (*entry).max(at);
    }
    let text = serde_json::to_string(&map).map_err(|e| format!("proj: {RELEASED}: {e}"))?;
    swap(&cfg.jobs.join(RELEASED), &format!("{text}\n"))
        .map_err(|e| format!("proj: {RELEASED}: {e}"))
}

fn read_admissions(cfg: &Config) -> BTreeMap<String, Admission> {
    let Some(text) = crate::read(&cfg.jobs.join(ADMISSION)) else {
        return BTreeMap::new();
    };
    let trimmed = text.trim();
    if trimmed.starts_with('{')
        && !trimmed.contains('\t')
        && let Ok(map) = serde_json::from_str::<BTreeMap<String, Admission>>(trimmed)
    {
        return map;
    }
    // Legacy single-line `<json-or-key>\t<cutoff>` format belongs to `claude`.
    let mut map = BTreeMap::new();
    if let Some((key, cutoff)) = trimmed.split_once('\t') {
        if let Ok(a) = serde_json::from_str::<Admission>(key) {
            map.insert(DEFAULT_ENGINE.to_owned(), a);
        } else if let Ok(c) = cutoff.trim().parse::<i64>() {
            map.insert(
                DEFAULT_ENGINE.to_owned(),
                Admission {
                    until: c / 1_000_000_000,
                    cutoff: c,
                    committed: true,
                },
            );
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
    let mut map = read_admissions(cfg);
    map.insert(
        engine.to_owned(),
        Admission {
            until: s.until,
            cutoff,
            committed,
        },
    );
    let text = serde_json::to_string(&map).map_err(|e| e.to_string())?;
    swap(&cfg.jobs.join(ADMISSION), &format!("{text}\n")).map_err(|e| e.to_string())?;
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
