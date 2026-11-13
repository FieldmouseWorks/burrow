//! The hold's two records, read as one. A usage or owner hold is kept in
//! [`FILE`], which earlier binaries read and write too, and a login hold in
//! [`LOGIN_FILE`], which they never see. Each change writes one of them
//! and then removes the other, so a reader that looked at each once could
//! find neither (the usage record not yet written, the login one already
//! gone) though a hold stood throughout. And both are left when a crash
//! comes between, or an earlier binary writes its usage record while a
//! login hold is kept.
//!
//! So a read ([`read`]) is retried until it saw one state: the generation
//! ([`GENERATION`]), which every change here moves before it touches either
//! record, and the usage record, read before and after the login record,
//! both unchanged. An earlier binary's write changes only the usage
//! record, which that second read catches. No lock is taken: admission
//! reads while it holds the slots' census, and an emergency promotion reads
//! past a stalled usage transaction.
//!
//! When both records are there, which is the later is known without
//! comparing refusal times (which a delayed or same-second refusal makes
//! ambiguous): a login record keeps the usage record's text as it stood
//! when it was written ([`Spent::over`]), so a usage record that differs
//! from it was written since. The two are then reconciled ([`reconcile`]):
//! an owner hold that holds is the owner's alone; two unended holds are one,
//! the usage limit's (it has a reset, and was told at once) unless it had
//! lapsed when the login hold replaced it, held till the later of their
//! deadlines and refused as late as either was; an ended record ends the
//! other only if it ended after that one's last refusal; otherwise the
//! later record stands, its own release and admission boundary with it.

use std::fs;

use super::{
    ADMISSION_REF, Config, EARLIER, FILE, LOGIN_FILE, Spent, owner_generation, owner_metadata,
};
use crate::{now, swap};

/// Moved by every change to either record, before it touches one: a read
/// that finds it the same before and after saw no change of this binary's
/// in between.
pub(super) const GENERATION: &str = ".usage.generation";

/// How many reads a caller makes before it gives up: a writer's change is
/// a rename or two, so one more read finds it done.
const ATTEMPTS: usize = 100;

/// Its record, None when there's none, or why it couldn't be read: both
/// records, read as one state and reconciled.
pub(super) fn read(cfg: &Config) -> Result<Option<Spent>, String> {
    snapshot(cfg, |name| raw(cfg, name))
}

/// [`read`], through `raw` (a test's, to change the records between reads).
pub(super) fn snapshot(
    cfg: &Config,
    mut raw: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<Option<Spent>, String> {
    for _ in 0..ATTEMPTS {
        let before = raw(GENERATION)?;
        let usage = raw(FILE)?;
        let login = raw(LOGIN_FILE)?;
        let again = raw(FILE)?;
        let after = raw(GENERATION)?;
        if before == after && usage == again {
            return merged(cfg, usage, login, now());
        }
        std::thread::yield_now();
    }
    Err(format!(
        "proj: {FILE} and {LOGIN_FILE} changed under each of {ATTEMPTS} reads"
    ))
}

/// A file's whole text, None when it isn't there.
fn raw(cfg: &Config, name: &str) -> Result<Option<String>, String> {
    let path = cfg.jobs.join(name);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("proj: {}: {e}", path.display())),
    }
}

/// The two records' texts as one record.
fn merged(
    cfg: &Config,
    usage: Option<String>,
    login: Option<String>,
    now: i64,
) -> Result<Option<Spent>, String> {
    let u = usage.as_deref().map(|t| parse(cfg, FILE, t)).transpose()?;
    let l = login
        .as_deref()
        .map(|t| parse(cfg, LOGIN_FILE, t))
        .transpose()?;
    let mut s = match (u, l) {
        (Some(u), Some(l)) => {
            let login_later = l.over == usage;
            Some(reconcile(u, l, login_later, now))
        }
        (u, l) => u.or(l),
    };
    if let Some(s) = &mut s {
        s.over = None;
    }
    Ok(s)
}

/// The usage record `u` and the login record `l`, both kept, as one: see
/// the module's doc. `login_later` when `l` was written after `u`.
pub(super) fn reconcile(u: Spent, l: Spent, login_later: bool, now: i64) -> Spent {
    if u.owner && u.holds(now) {
        return u;
    }
    let base_is_login = login_later && !u.holds(now);
    let (later, earlier) = if login_later { (l, u) } else { (u, l) };
    match (later.back, earlier.back) {
        (None, None) if !later.owner && !earlier.owner => {
            // The login record is the base only when it's the later.
            let (mut base, other) = if base_is_login == login_later {
                (later, earlier)
            } else {
                (earlier, later)
            };
            base.until = base.until.max(other.until);
            base.last = Some(base.refused().max(other.refused()));
            base
        }
        (Some(back), None) if !earlier.owner && earlier.refused() >= back => earlier,
        _ => later,
    }
}

/// One record's text, as the hold it says.
fn parse(cfg: &Config, name: &str, text: &str) -> Result<Spent, String> {
    let path = cfg.jobs.join(name);
    let mut s = serde_json::from_str::<Spent>(text)
        .map_err(|e| format!("proj: {}: {e}", path.display()))?;
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
    s.login = name == LOGIN_FILE;
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
}

/// The generation moved on ([`GENERATION`]), before a record is changed.
fn advance(cfg: &Config) -> Result<(), String> {
    let path = cfg.jobs.join(GENERATION);
    let n = raw(cfg, GENERATION)?
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0);
    swap(&path, &format!("{}\n", n.wrapping_add(1)))
        .map_err(|e| format!("proj: {}: {e}", path.display()))
}

/// The record written whole (a temp file renamed over it), and the other
/// cause's removed, or why not. A login record keeps the usage record's
/// text as it stood ([`Spent::over`]), so a crash before that removal, or
/// an earlier binary's usage record written after, is told apart.
pub(super) fn keep(cfg: &Config, s: &Spent) -> Result<(), String> {
    let mut mirror = if s.owner && s.back.is_none() {
        owner_generation(cfg, s)?
    } else {
        s.clone()
    };
    if s.owner {
        mirror.last = Some(s.until - 1);
    }
    if let Some(cutoff) = s.admission {
        mirror.job = format!("{ADMISSION_REF}{cutoff}-{}", mirror.job);
    }
    let (name, other) = if s.login {
        (LOGIN_FILE, FILE)
    } else {
        (FILE, LOGIN_FILE)
    };
    mirror.over = if s.login { raw(cfg, FILE)? } else { None };
    let text = serde_json::to_string(&mirror).map_err(|e| format!("proj: {name}: {e}"))?;
    advance(cfg)?;
    swap(&cfg.jobs.join(name), &format!("{text}\n")).map_err(|e| format!("proj: {name}: {e}"))?;
    match fs::remove_file(cfg.jobs.join(other)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("proj: {other}: {e}")),
        _ => Ok(()),
    }
}

/// The record gone, whichever kind it is.
pub(super) fn forget(cfg: &Config) {
    if advance(cfg).is_err() {
        return;
    }
    for name in [FILE, LOGIN_FILE] {
        let _ = fs::remove_file(cfg.jobs.join(name));
    }
}
