//! The machine out of Claude usage. When Claude Code refuses a run because
//! the plan's usage is spent (a weekly or five-hour allowance, or an extra-usage
//! cap), every other run is refused the same way until it resets: from
//! 2026-11-01 to 11-04 every job's turn ended at once, 1,391 of them, each
//! retried and reported as an error, the cause named nowhere. So a refused
//! run ([`refused`]) puts the whole machine out of usage until the limit
//! resets ([`record`], in jobs/.usage.json): no turn takes a slot meanwhile
//! (slot.rs), the turn that was refused waits in line again and runs once
//! more after (turn.rs), the stall watchdog holds what waits on it (`proj
//! usage` tells it), and the owner hears once, with the cause and when it
//! resets. The first run that started after the refusal and isn't refused
//! ends it ([`back`]), and the notice is closed; so does `proj usage back`,
//! once usage is available before the recorded reset. Scheduling also ends
//! ordinary usage holds through `back` when a fresh meter read after the last
//! refusal has room. An explicit owner hold (`proj usage hold`) instead lasts
//! until its deadline or `usage back`; running turns and fresh meters cannot
//! release it early.
//!
//! Every change to it goes under jobs/.usage.lock, so turns refused at once,
//! and runs that finish meanwhile, each see the last one's. Its record
//! outlives the hold: until the owner's notice is surely closed, and for a
//! stall's time after the line moved again, which the watchdog's deadlines
//! for turns in line count from ([`Spent::release`]). HOST-notify can't say
//! whether GitHub took a notice, so the open is sent again every RETELL
//! while it holds ([`remind`]), and the close until `HOST-notify pending`
//! says it got through: by the runs, and by proj-watch's every look, which
//! books its next for it ([`tend`]), so neither waits on a run to come.

use std::fs::{self, File};
use std::process::{Command, Stdio};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::job::{Job, kick};
use crate::waits::rewatch_all;
use crate::{filled, mtime, now, swap};

/// Where it's kept, in jobs.
pub(crate) const FILE: &str = ".usage.json";
/// When a hold last ended, in jobs: kept for good, past the record (which
/// goes once its notice is settled, or is replaced by a new refusal), so an
/// earlier proj's limit written before it never holds again ([`earlier`]).
const RELEASED: &str = ".usage.released";
const OWNER: &str = ".usage-owner-";
const ADMISSION_REF: &str = ".usage-release-";
/// The owner's notice's key.
const NOTICE: &str = "proj-usage";
/// How long it's out when Claude Code doesn't say when the limit resets,
/// or says a time already past: it's tried again then.
const UNSAID: i64 = 3600;
/// How often the owner's notice is sent again while it may not have got
/// through: HOST-notify never says.
const RETELL: i64 = 3600;
/// How soon proj-watch looks at the notice again when it couldn't be
/// tended (the lock or the record couldn't be had, or written).
const RETRY: i64 = RETELL / 12;

/// The machine out of usage: until when, Claude Code's own words, and the
/// job and time it was refused at.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    /// An explicit owner pause: only the owner or its deadline releases it.
    #[serde(skip)]
    pub owner: bool,
    pub until: i64,
    pub said: String,
    pub job: String,
    pub at: i64,
    /// When it ended, by a run that wasn't refused or `proj usage back`;
    /// None while it holds.
    #[serde(default)]
    pub back: Option<i64>,
    /// Precise admission boundary, committed atomically with `back`. A cutoff
    /// from an unsuccessful release attempt must never affect an active hold.
    #[serde(default, skip_serializing)]
    pub admission: Option<i64>,
    /// When the owner's notice was last sent: its open while it holds, its
    /// close after.
    #[serde(default)]
    pub told: i64,
    /// When a run was last refused while it held: only a run that started
    /// after that says usage is back ([`back`]). An earlier proj's limit
    /// taken in ([`earlier`]) counts from when that was written, not from
    /// when it was taken, and from 0 when it doesn't say. None in a record
    /// from before it was kept, which `at` stands for.
    #[serde(default)]
    pub last: Option<i64>,
}

impl Spent {
    /// When the line moved again: its reset, or sooner if it ended sooner.
    pub fn release(&self) -> i64 {
        self.back.map_or(self.until, |b| b.min(self.until))
    }

    fn admission_cutoff(&self) -> i64 {
        if self.back.is_some() {
            self.admission.unwrap_or(self.release() * 1_000_000_000)
        } else {
            self.until * 1_000_000_000
        }
    }

    /// Whether it holds the line at `now`.
    pub fn holds(&self, now: i64) -> bool {
        self.back.is_none() && self.until > now
    }

    /// Expiry suppresses only pre-deadline imports; explicit releases are inclusive.
    fn release_cutoff(&self) -> Option<i64> {
        self.back.map(|back| {
            if self.owner && back == self.until {
                back - 1
            } else {
                back
            }
        })
    }

    /// When a run was last refused: a run that started then or before
    /// proves nothing of usage now.
    pub fn refused(&self) -> i64 {
        self.last.unwrap_or(self.at)
    }
}

/// The machine's state as it stands: Some while it's out of usage.
pub fn out(cfg: &Config) -> Option<Spent> {
    held(cfg).ok().flatten()
}

/// The same, or why its record (or the owner hold it refers to) couldn't
/// be read: for what must say so rather than take it for no hold (`proj
/// settings show`). Reads only.
pub fn held(cfg: &Config) -> Result<Option<Spent>, String> {
    Ok(read(cfg)?.filter(|s| s.holds(now())))
}

/// A scheduling pass can discover usage is back without starting a refused
/// turn: only a fresh Claude meter read after the last refusal proves it.
/// `back` checks that timestamp again under the usage lock, so a refusal
/// arriving meanwhile keeps its hold. All release effects still live there.
pub fn metered(cfg: &Config) {
    if let Some(started) = meter_start(cfg) {
        back(cfg, Some(started));
    }
}

fn meter_start(cfg: &Config) -> Option<i64> {
    use burrow_usage_reset::meter::{FRESH, Snapshot};
    let s = out(cfg)?;
    let dir = cfg.meters.as_ref()?;
    let now = now() as f64;
    let meter = Snapshot::read(dir, "claude", now)?;
    let requested = meter.requested_at?;
    if !requested.is_finite() || requested > meter.read_at || now - requested >= FRESH {
        return None;
    }
    // Refusals are recorded in whole seconds. Discard the fractional part:
    // a request within the refusal's second could have preceded it. Pass
    // this same conservative time to back's locked check of the latest refusal.
    let started = requested.floor() as i64;
    (started > s.refused() && meter.room()).then_some(started)
}

/// The actual release, retained after the usage notice and record are gone.
pub fn last_release(cfg: &Config) -> Option<i64> {
    let now = now();
    if let Some(spent) = kept(cfg) {
        return (spent.release() <= now).then(|| spent.release());
    }
    released(cfg).ok().filter(|at| *at > 0)
}

/// Admission's nanosecond cutoff, independent of the later successful-run
/// watermark used to reject stale refusal records. Serialized with usage changes.
pub fn admission_cutoff(cfg: &Config) -> std::io::Result<Option<i64>> {
    let _lock = try_lock(cfg)?;
    admission_cutoff_locked(cfg)
}

#[derive(Deserialize, Serialize)]
struct Admission {
    until: i64,
    cutoff: i64,
    committed: bool,
}

fn admission_cutoff_locked(cfg: &Config) -> std::io::Result<Option<i64>> {
    if let Some(s) = read(cfg).map_err(std::io::Error::other)? {
        if s.holds(now()) {
            return Ok(None);
        }
        if s.back.is_some() {
            let raw: Spent = serde_json::from_slice(&fs::read(cfg.jobs.join(FILE))?)?;
            if !raw.job.starts_with(ADMISSION_REF) {
                // Adopt an old writer's release once, while holding its lock.
                // Later legacy notice writes preserve this reference instead
                // of moving the cohort to the notice's modification time.
                write_admission(cfg, &s, false)
                    .and_then(|_| keep(cfg, &s))
                    .and_then(|_| admission_release(cfg, &s).map(|_| ()))
                    .map_err(std::io::Error::other)?;
            }
        }
        return Ok(Some(s.admission_cutoff()));
    }
    let Some(text) = crate::read(&cfg.jobs.join(".usage.admission")) else {
        return Ok(None);
    };
    let cutoff = text.split_once('\t').and_then(|(key, cutoff)| {
        if let Ok(a) = serde_json::from_str::<Admission>(key) {
            // An old generation may settle after a crash before the journal
            // acknowledgement. Include the original natural cohort then.
            Some(if a.committed {
                a.cutoff
            } else {
                a.until * 1_000_000_000
            })
        } else {
            cutoff.trim().parse().ok()
        }
    });
    Ok(cutoff)
}

fn write_admission(cfg: &Config, s: &Spent, committed: bool) -> Result<i64, String> {
    let cutoff = s.admission_cutoff();
    let state = Admission {
        until: s.until,
        cutoff,
        committed,
    };
    let text = serde_json::to_string(&state).map_err(|e| e.to_string())?;
    swap(
        &cfg.jobs.join(".usage.admission"),
        &format!("{text}\t{cutoff}\n"),
    )
    .map_err(|e| e.to_string())?;
    Ok(cutoff)
}

fn admission_release(cfg: &Config, s: &Spent) -> Result<i64, String> {
    write_admission(cfg, s, true)
}

/// Its record, holding or not, while it's kept.
pub fn kept(cfg: &Config) -> Option<Spent> {
    read(cfg).ok().flatten()
}

/// Its record, None when there's none, or why it couldn't be read.
fn read(cfg: &Config) -> Result<Option<Spent>, String> {
    let path = cfg.jobs.join(FILE);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Spent>(&text)
            .map(|mut s| {
                if s.back.is_some()
                    && let Some((cutoff, job)) = s
                        .job
                        .strip_prefix(ADMISSION_REF)
                        .and_then(|reference| reference.split_once('-'))
                    && let Ok(cutoff) = cutoff.parse::<i64>()
                {
                    s.admission = Some(cutoff);
                    s.job = job.to_owned();
                }
                if let Some(back) = s.back
                    && s.admission.is_none()
                {
                    // An old writer supplies only a whole-second release.
                    // Its filesystem timestamp may be coarser than new queue
                    // timestamps. Include the whole release second so none of
                    // its same-second waiters can bypass the cap.
                    s.admission = Some(((back + 1) * 1_000_000_000).min(s.until * 1_000_000_000));
                }
                // Legacy writers preserve the metadata reference in job. Their
                // last-refusal update distinguishes a real refusal at/after the
                // deadline from the compatibility fence (deadline minus one).
                if let Some(mut owner) = owner_metadata(cfg, &s.job)? {
                    if s.refused() >= owner.until {
                        s.said = EARLIER.into();
                    } else {
                        owner.owner = true;
                        owner.told = s.told;
                        owner.back = s.back;
                        owner.admission = s.admission;
                        return Ok(owner);
                    }
                }
                s.said = crate::engine::claude::normalize_refusal(&s.said);
                Ok(s)
            })
            .map_err(|e| format!("proj: {}: {e}", path.display()))?
            .map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("proj: {}: {e}", path.display())),
    }
}

/// When a hold last ended ([`RELEASED`]), 0 if none has; or why it
/// couldn't be read, when it's there.
fn released(cfg: &Config) -> Result<i64, String> {
    let path = cfg.jobs.join(RELEASED);
    match fs::read_to_string(&path) {
        Ok(text) => text
            .trim()
            .parse()
            .map_err(|e| format!("proj: {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!("proj: {}: {e}", path.display())),
    }
}

/// jobs/.usage.lock, held: every change to the record goes under it, and
/// none goes ahead without it.
pub(crate) fn lock(cfg: &Config) -> Result<crate::held::Held, String> {
    let path = cfg.jobs.join(".usage.lock");
    let f = File::create(&path).map_err(|e| format!("proj: {}: {e}", path.display()))?;
    crate::held::Held::lock(f).map_err(|e| format!("proj: {}: {e}", path.display()))
}

/// Waiters retry contention so an emergency promotion can bypass a stalled
/// usage transaction. Other I/O failures retain their original meaning.
///
/// One try can find the lock taken when nobody holds it: a child any thread
/// of the process forks holds a copy of every descriptor open at the fork,
/// a held lock's included, until it execs, so for that moment a lock its
/// holder has let go still looks taken (#675). A caller holding another
/// lock (admission holds the slots' census) tries once and retries from its
/// own loop, letting that lock go meanwhile; one that holds none waits the
/// moment out ([`wait_lock`]).
pub(crate) fn try_lock(cfg: &Config) -> std::io::Result<File> {
    let f = File::create(cfg.jobs.join(".usage.lock"))?;
    f.try_lock().map_err(std::io::Error::from)?;
    Ok(f)
}

/// How long a caller that holds no other lock waits out contention before
/// it takes the lock for taken: a fork's moment, and short enough that a
/// stalled usage transaction holds it up no longer than that.
const CONTENTION: std::time::Duration = std::time::Duration::from_secs(1);

/// [`try_lock`], for a caller holding no other lock: WouldBlock once the
/// lock has been taken for [`CONTENTION`].
fn wait_lock(cfg: &Config) -> std::io::Result<File> {
    let deadline = std::time::Instant::now() + CONTENTION;
    loop {
        match try_lock(cfg) {
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            got => return got,
        }
    }
}

fn owner_reference(job: &str) -> bool {
    job.strip_prefix(OWNER).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn owner_metadata(cfg: &Config, job: &str) -> Result<Option<Spent>, String> {
    if !owner_reference(job) {
        return Ok(None);
    }
    let bytes = fs::read(cfg.jobs.join(job)).map_err(|e| format!("proj: {job}: {e}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("proj: {job}: {e}"))
}

/// Under the usage lock, persist expiry's stale-import cutoff before any
/// writer can replace its reference. The cutoff is inclusive, so expiry at
/// UNTIL suppresses imports before UNTIL while preserving refusals at UNTIL.
fn reconcile_expiry(cfg: &Config) -> Result<(), String> {
    let Some(mut spent) = read(cfg)? else {
        return Ok(());
    };
    // An explicitly released owner already recorded its own cutoff. Its
    // original deadline cannot later supersede that earlier release.
    if spent.owner && spent.back.is_some() {
        return Ok(());
    }
    let Some(owner) = owner_metadata(cfg, &spent.job)? else {
        return Ok(());
    };
    if owner.until > now() {
        return Ok(());
    }
    let cutoff = owner.until - 1;
    let newly = released(cfg)? < cutoff;
    if newly {
        mark(cfg, cutoff)?;
    }
    if spent.owner && spent.back.is_none() {
        spent.back = Some(spent.until);
        spent.told = now();
        keep(cfg, &spent)?;
        tell(cfg, &["close", NOTICE, CLOSED]);
        changed(cfg);
    } else if newly && spent.back.is_none() {
        // A legacy refusal replaced the owner in the compatible record.
        // Keep that genuine limit and publish its current cause/deadline.
        tell(cfg, &["open", NOTICE, &title(&spent), &body(cfg, &spent)]);
        changed(cfg);
    }
    Ok(())
}

/// Prepare immutable metadata first; publishing the compatible record is the
/// only commit point. Old generations remain readable by concurrent readers.
fn owner_generation(cfg: &Config, s: &Spent) -> Result<Spent, String> {
    if owner_reference(&s.job)
        && let Ok(bytes) = fs::read(cfg.jobs.join(&s.job))
        && let Ok(previous) = serde_json::from_slice::<Spent>(&bytes)
        && previous.until == s.until
        && previous.at == s.at
        && previous.said == s.said
    {
        return Ok(s.clone());
    }
    let mut file = tempfile::Builder::new()
        .prefix(OWNER)
        .tempfile_in(&cfg.jobs)
        .map_err(|e| format!("proj: owner metadata: {e}"))?;
    let mut generation = s.clone();
    generation.job = file
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let text =
        serde_json::to_string(&generation).map_err(|e| format!("proj: owner metadata: {e}"))?;
    std::io::Write::write_all(file.as_file_mut(), text.as_bytes())
        .map_err(|e| format!("proj: owner metadata: {e}"))?;
    file.keep()
        .map_err(|e| format!("proj: owner metadata: {e}"))?;
    Ok(generation)
}

/// The record written whole (a temp file renamed over it), or why not.
fn keep(cfg: &Config, s: &Spent) -> Result<(), String> {
    let mut mirror = if s.owner && s.back.is_none() {
        owner_generation(cfg, s)?
    } else {
        s.clone()
    };
    if s.owner {
        mirror.last = Some(s.until - 1);
    }
    if let Some(cutoff) = s.admission {
        mirror.job = format!("{ADMISSION_REF}{cutoff}-{}", s.job);
    }
    let text = serde_json::to_string(&mirror).map_err(|e| format!("proj: {FILE}: {e}"))?;
    swap(&cfg.jobs.join(FILE), &format!("{text}\n")).map_err(|e| format!("proj: {FILE}: {e}"))
}

/// Job `id`'s run was refused at `at`: the machine is out of usage until
/// `until`, or UNSAID on when that's unsaid or past. The owner is told when
/// it wasn't out already, or its reset had passed (a run refused again then
/// is a new one to tell), and every turn in line has its deadline moved.
/// What it's out until, as recorded, or why it couldn't be.
pub fn record(
    cfg: &Config,
    id: &str,
    until: Option<i64>,
    said: &str,
    at: i64,
) -> Result<Spent, String> {
    let _lock = lock(cfg)?;
    let stale_owner_refusal = read(cfg)?
        .map(|s| {
            if s.owner && s.back.is_some_and(|back| back < s.until) {
                Ok(None)
            } else {
                owner_metadata(cfg, &s.job)
            }
        })
        .transpose()?
        .flatten()
        .is_some_and(|owner| owner.until <= now() && at < owner.until);
    reconcile_expiry(cfg)?;
    if stale_owner_refusal {
        return read(cfg)?
            .ok_or_else(|| "proj: expired owner record disappeared under its lock".into());
    }
    put(cfg, id, until, said, at, at)
}

/// A fresh owner pause, independent of releases of earlier usage limits.
pub fn hold(cfg: &Config, until: i64, reason: &str) -> Result<(), String> {
    let _lock = lock(cfg)?;
    let at = now();
    if until <= at {
        return Err("proj usage hold: UNTIL must be a future Unix timestamp".into());
    }
    reconcile_expiry(cfg)?;
    let spent = Spent {
        owner: true,
        until,
        said: reason.to_owned(),
        job: "owner".into(),
        at,
        back: None,
        admission: None,
        told: at,
        last: Some(at),
    };
    keep(cfg, &spent)?;
    tell(cfg, &["open", NOTICE, &title(&spent), &body(cfg, &spent)]);
    changed(cfg);
    Ok(())
}

/// [`record`], under the lock, with `last` when it was refused: only a run
/// started after that ends it.
fn put(
    cfg: &Config,
    id: &str,
    until: Option<i64>,
    said: &str,
    at: i64,
    last: i64,
) -> Result<Spent, String> {
    let until = until.filter(|&t| t > at).unwrap_or(at + UNSAID);
    if let Some(owner) = kept(cfg).filter(|w| w.owner && w.holds(at)) {
        return Ok(owner);
    }
    let was = kept(cfg).filter(|w| w.back.is_none() && !w.owner);
    let lapsed = was.as_ref().is_none_or(|w| !w.holds(at));
    let spent = match &was {
        // Every running turn is refused at once: the first tells the owner,
        // the rest move the reset on, and when it was last refused.
        Some(w) => Spent {
            until: w.until.max(until),
            last: Some(w.refused().max(last)),
            told: if lapsed { now() } else { w.told },
            ..w.clone()
        },
        None => Spent {
            owner: false,
            until,
            said: crate::engine::claude::normalize_refusal(said),
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
    keep(cfg, &spent)?;
    if lapsed {
        tell(cfg, &["open", NOTICE, &title(&spent), &body(cfg, &spent)]);
    }
    if lapsed || was.as_ref().is_some_and(|w| w.until != spent.until) {
        changed(cfg);
    }
    Ok(spent)
}

/// The hold changed: every turn in line has its deadline moved, the stall
/// watchdog looks again whether or not those did (it holds every other
/// source's waits on Claude by it too), and so does proj-watch, which tends the
/// owner's notice from then on ([`tend`]).
fn changed(cfg: &Config) {
    rewatch_all(cfg);
    let mut kicks = vec!["proj-watch".to_owned()];
    if cfg.waits.is_some() {
        kicks.push("watchdog".to_owned());
    }
    kick(cfg, kicks);
}

/// While it holds, the owner's notice once more every RETELL: one GitHub
/// didn't take is sent again. Cheap when there's nothing to do, so every
/// turn in line can ask.
pub fn remind(cfg: &Config) {
    let due = |s: &Spent| s.holds(now()) && now() - s.told >= RETELL;
    if !kept(cfg).is_some_and(|s| due(&s)) {
        return;
    }
    let Ok(_lock) = wait_lock(cfg) else { return };
    if let Err(e) = reconcile_expiry(cfg) {
        eprintln!("{e}");
        return;
    }
    if let Some(mut s) = kept(cfg).filter(|s| due(s)) {
        s.told = now();
        if keep(cfg, &s).is_ok() {
            tell(cfg, &["open", NOTICE, &title(&s), &body(cfg, &s)]);
        }
    }
}

/// Usage is back: a run that started at `started` (in seconds) and wasn't
/// refused, or `proj usage back` (None: usage is available sooner). The
/// turns waiting take slots again, every turn in line has its deadline
/// moved, and the owner's notice is closed. An owner hold cannot be ended
/// early by a successful run. A run that started no later
/// than the last refusal proves nothing: its last call may have come before
/// it. Once it has ended, each call settles what's left ([`settle`]). The
/// owner's release is kept ([`RELEASED`]) with or without a hold to end,
/// one settled or none recorded yet: an earlier proj's or watchdog's limit
/// taken in after it, but written before, never holds again ([`earlier`]).
pub fn back(cfg: &Config, started: Option<i64>) {
    let owner = started.is_none();
    if !owner && kept(cfg).is_none() {
        return;
    }
    let Ok(_lock) = lock(cfg) else { return };
    if let Err(e) = reconcile_expiry(cfg) {
        eprintln!("{e}");
        return;
    }
    let now = now();
    let s = kept(cfg);
    let ends = s.as_ref().is_some_and(|s| s.back.is_none());
    if !owner && s.as_ref().is_some_and(|s| s.owner && s.holds(now)) {
        return;
    }
    if ends && started.is_some_and(|t| s.as_ref().is_some_and(|s| t <= s.refused())) {
        return;
    }
    // What it ended is kept first, past the record: without it, an earlier
    // proj's limit could hold it again.
    if (owner || ends)
        && let Err(e) = mark(cfg, now)
    {
        eprintln!("{e}");
        return;
    }
    match s {
        Some(mut s) if ends => {
            s.admission = Some(crate::now_nanos().min(s.until * 1_000_000_000));
            s.back = Some(now);
            s.told = now;
            if let Err(e) = write_admission(cfg, &s, false).and_then(|_| keep(cfg, &s)) {
                eprintln!("{e}");
                return;
            }
            // The compatible record is the commit point. A failed journal
            // acknowledgement leaves it intact for settlement to retry.
            let _ = admission_release(cfg, &s);
            eprintln!("proj: Claude has usage again (out since {})", when(s.at));
            tell(cfg, &["close", NOTICE, CLOSED]);
            changed(cfg);
        }
        Some(s) => {
            settle(cfg, s, now);
            if owner {
                changed(cfg);
            }
        }
        // The watchdog, and proj-watch's look, drop the limits they still
        // keep that it ended.
        None => {
            eprintln!("proj: no usage hold is on; no usage limit recorded before now will hold");
            changed(cfg);
        }
    }
}

/// A release at `now`, kept in [`RELEASED`] for good (the latest one), or
/// why not.
fn mark(cfg: &Config, now: i64) -> Result<(), String> {
    let r = released(cfg)?.max(now);
    swap(&cfg.jobs.join(RELEASED), &format!("{r}\n")).map_err(|e| format!("proj: {RELEASED}: {e}"))
}

/// The owner's notice, with no run to ask (proj-watch's every look): while
/// it holds, its open again every RETELL ([`remind`]); once it has ended,
/// what's left settled ([`settle`]). When it next needs a look, while
/// there's anything left: one past its reset that no run has ended yet
/// waits for the next run that isn't refused, which closes it.
/// What couldn't be done (the lock or the record not had, a send or the
/// record's removal not written) is looked at again in RETRY, never
/// sooner, so the look neither forgets it nor spins on it.
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
    let s = match read(cfg) {
        Ok(s) => s?,
        Err(e) => {
            eprintln!("{e}");
            return retry;
        }
    };
    let next = if s.holds(now) {
        remind(cfg);
        kept(cfg).map(|s| {
            if s.owner {
                (s.told + RETELL).min(s.until)
            } else {
                s.told + RETELL
            }
        })
    } else {
        s.back?;
        let Ok(_lock) = lock(cfg) else { return retry };
        let s = kept(cfg).filter(|s| s.back.is_some())?;
        settle(cfg, s, now);
        let s = kept(cfg)?;
        Some(if pending(cfg) {
            s.told + RETELL / 12
        } else {
            s.release() + cfg.stall_queued()
        })
    };
    let floor = if s.owner && s.holds(now) {
        now + 1
    } else {
        now + RETRY
    };
    Some(next.unwrap_or(now).max(floor))
}

/// What's left once it has ended, under the lock: the close sent again
/// until it got through, and the record gone a stall's time after the
/// release.
fn settle(cfg: &Config, mut s: Spent, now: i64) {
    if admission_release(cfg, &s).is_err() {
        return;
    }
    let settled = !pending(cfg);
    if settled && now >= s.release() + cfg.stall_queued() {
        let _ = fs::remove_file(cfg.jobs.join(FILE));
    } else if !settled && now - s.told >= RETELL / 12 {
        s.told = now;
        if keep(cfg, &s).is_ok() {
            tell(cfg, &["close", NOTICE, CLOSED]);
        }
    }
}

/// What an earlier proj's own record of a usage limit is said as, once
/// it's taken into the hold.
pub const EARLIER: &str = "a usage limit an earlier proj or watchdog recorded";

/// A usage limit an earlier proj or watchdog recorded apart from this one,
/// written at `written` and resetting at `reset`, taken into the hold: none
/// once it has reset, nor when any release since (`proj usage back`, or a
/// run that wasn't refused) has ended a hold, which [`RELEASED`] keeps for
/// good. Its refusal counts from `written`, so a run started since then
/// ends it. A limit that doesn't say when it was written (#428's
/// watchdog's: None) was written before this proj ran, so before any
/// release it made, and counts from 0. Taking one again changes nothing.
/// Each caller drops its own record of it only once this has succeeded.
pub fn earlier(cfg: &Config, id: &str, reset: i64, written: Option<i64>) -> Result<(), String> {
    let _lock = lock(cfg)?;
    earlier_locked(cfg, id, reset, written)
}

fn earlier_locked(cfg: &Config, id: &str, reset: i64, written: Option<i64>) -> Result<(), String> {
    reconcile_expiry(cfg)?;
    let released = released(cfg)?.max(read(cfg)?.and_then(|s| s.release_cutoff()).unwrap_or(0));
    let ended = match written {
        Some(w) => w <= released,
        None => released > 0,
    };
    if reset <= now() || ended {
        return Ok(());
    }
    put(cfg, id, Some(reset), EARLIER, now(), written.unwrap_or(0)).map(|_| ())
}

/// The holds an earlier proj (#428's) kept apart from this one, each taken
/// into it ([`earlier`]): N.resets, the time a job's latest turn wrote that
/// its usage limit resets, any job's (an --owned one's included, whose
/// starter would have asked `proj resets`), and from a turn still running
/// that proj once it ends. That turn writes N.resets before its outcome, so
/// one with no outcome yet may be half written and is left for a later
/// look. All under the lock, so two looks never take one twice, and each
/// removed only once the hold has it. The stall watchdog asks `proj usage`
/// every pass, which runs this first.
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
        let f = job.turn(&n, "resets");
        if !f.exists() || !filled(&job.turn(&n, "outcome")) {
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

const CLOSED: &str = "Claude has usage again, and proj's jobs are running.";

/// Whether HOST-notify may still have the notice open: a close that didn't
/// get through. Only its exit 1 says it's closed (or was never opened); one
/// that couldn't run, was killed or answered anything else may still have
/// it. Without HOST-notify, nothing is.
fn pending(cfg: &Config) -> bool {
    cfg.notify.as_ref().is_some_and(|n| {
        !Command::new(n)
            .args(["pending", NOTICE])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|st| st.code() == Some(1))
    })
}

/// It, in a line: `proj status`'s, and the turn's journal's.
pub fn line(s: &Spent) -> String {
    hold_line(s.owner, s.until, &s.said)
}

/// A hold in a line, from what the live snapshot keeps of it: the owner's
/// pause or a refusal, until when, and its reason or Claude Code's words.
pub fn hold_line(owner: bool, until: i64, said: &str) -> String {
    if owner {
        return format!(
            "Claude work is paused by the owner until {}: {said}",
            when(until)
        );
    }
    format!(
        "Claude is out of usage until {}: no Claude turn starts till then (\"{said}\")",
        when(until)
    )
}

fn title(s: &Spent) -> String {
    if s.owner {
        return format!("Claude work paused by the owner until {}", when(s.until));
    }
    format!("Claude is out of usage until {}", when(s.until))
}

fn body(cfg: &Config, s: &Spent) -> String {
    if s.owner {
        return format!(
            "The owner paused Claude work until {}. Reason: {}. Running turns continue; Codex work remains eligible. Use `proj usage back` to release this pause early.",
            when(s.until),
            s.said
        );
    }
    format!(
        "Since {}, Claude Code refuses every Claude job here. It answered job {}:\n\n> {}\n\n\
         proj starts no Claude turn until {}. The turns that were refused wait in line and run again then, by themselves, \
         and the stall watchdog holds what waits on them. {} \
         This closes once a job runs again.",
        when(s.at),
        s.job,
        s.said,
        when(s.until),
        policy(cfg)
    )
}

pub fn policy(cfg: &Config) -> &'static str {
    if cfg.usage_paid {
        "If usage is available sooner, `proj usage back` releases the hold."
    } else {
        "Paid extra usage is disabled. A usage limit means waiting until reset; never ask the owner to raise a spend cap or enable paid usage."
    }
}

/// A moment as the machine's clock (its time zone) says it.
pub fn when(t: i64) -> String {
    Timestamp::from_second(t).map_or_else(
        |_| t.to_string(),
        |at| {
            at.to_zoned(TimeZone::system())
                .strftime("%a %b %-d, %-I:%M %p %Z")
                .to_string()
        },
    )
}

/// HOST-notify, which never fails its caller; without it, the journal has
/// it.
fn tell(cfg: &Config, args: &[&str]) {
    let Some(notify) = &cfg.notify else {
        eprintln!("proj: {}", args.join(": "));
        return;
    };
    if let Err(e) = Command::new(notify).args(args).status() {
        eprintln!("proj: {}: {e}", notify.display());
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod owner_tests;
