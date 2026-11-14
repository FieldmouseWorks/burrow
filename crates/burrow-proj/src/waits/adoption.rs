//! Durable recovery of waits lost before policy holds existed. Selection
//! uses a saved policy generation; publication always uses current job facts.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(super) const ADOPTED: &str = ".adopted";
const PENDING: &str = ".adopting";

#[derive(Deserialize, Serialize)]
struct Adoption {
    policies: Vec<Option<burrow_settings::Snapshot>>,
    #[serde(default)]
    refresh: bool,
    found: BTreeSet<String>,
}

impl Adoption {
    fn load(cfg: &Config, path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let mut adoption: Self = serde_json::from_slice(&bytes)?;
                if adoption.policies.is_empty() {
                    return Err(io::Error::other("adoption has no policy snapshot"));
                }
                if adoption.refresh {
                    adoption.policies.push(Self::snapshot(cfg)?);
                    adoption.refresh = false;
                    adoption.save(path)?;
                }
                Ok(adoption)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let adoption = Self {
                    policies: vec![Self::snapshot(cfg)?],
                    refresh: false,
                    found: BTreeSet::new(),
                };
                // Commit eligibility before any job read can fail or policy
                // changes. Every retry uses this same snapshot and clock.
                adoption.save(path)?;
                Ok(adoption)
            }
            Err(e) => Err(e),
        }
    }

    fn snapshot(cfg: &Config) -> io::Result<Option<burrow_settings::Snapshot>> {
        cfg.settings
            .as_ref()
            .map(|s| s.snapshot())
            .transpose()
            .map_err(io::Error::other)
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        swap(path, &serde_json::to_string(self)?)
    }
}

pub(super) fn rewatch_all(cfg: &Config) -> bool {
    loop {
        if let Some(result) = forward(cfg, "_rewatch_all", &[]) {
            return result;
        }
        let result = local(cfg);
        // local has dropped every lock before a direct or chained handoff.
        if let Some(result) = forward(cfg, "_rewatch_all", &[]) {
            return result;
        }
        match result {
            Ok(true) => return true,
            Ok(false) => continue, // Deployment moved and then came back.
            Err(e) => {
                eprintln!("proj: couldn't refresh the stall watchdog's waits: {e}");
                return false;
            }
        }
    }
}

fn local(cfg: &Config) -> io::Result<bool> {
    let _guard = lock(cfg)?;
    // A retained caller may have waited behind the current deployment.
    if successor(cfg).is_some() {
        return Ok(false);
    }
    let held = cfg.jobs.join(HELD);
    fs::create_dir_all(&held)?;
    let adopted = held.join(ADOPTED);
    let pending = held.join(PENDING);
    let mut migration = if cfg.waits.is_some() && !adopted.try_exists()? {
        Some(Adoption::load(cfg, &pending)?)
    } else {
        None
    };
    // Record a disabled interval before discovery can block. If deployment
    // changes during the census, its enabling owner must see the reset.
    if cfg.waits.is_none() {
        publish(cfg, &[])?;
    }
    let mut complete = true;
    if let Some(migration) = &mut migration {
        let (found, scanned) = adoptable(cfg, &migration.policies);
        migration.found.extend(found);
        migration.save(&pending)?;
        complete = scanned;
    }
    let mut ids: BTreeSet<String> = [".waits", HELD]
        .iter()
        .flat_map(|d| fs::read_dir(cfg.jobs.join(d)).into_iter().flatten())
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.ends_with(".new"))
        .collect();
    if let Some(migration) = &migration {
        ids.extend(migration.found.iter().cloned());
    }
    // One systemd census for liveness; settled jobs need no per-job call.
    let up = crate::systemd::up(cfg, "proj-*.service");
    ids.extend(
        fs::read_dir(&cfg.jobs)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.') && live(cfg, n, up.as_ref())),
    );
    // Deployment may have changed while discovery was reading job state.
    if successor(cfg).is_some() {
        return Ok(false);
    }
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    if cfg.waits.is_some() {
        publish(cfg, &ids)?;
    }
    if successor(cfg).is_some() {
        return Ok(false);
    }
    if migration.is_some() {
        if !complete {
            return Err(io::Error::other(
                "legacy wait discovery is incomplete; retrying next pass",
            ));
        }
        fs::write(adopted, "")?;
        fs::remove_file(pending)?;
    }
    Ok(true)
}

/// A completed transaction leaves no eligibility to retain. An unfinished
/// one keeps its old snapshots, and adds the next enabled interval's snapshot
/// before scanning again, so neither interval loses unreadable legacy jobs.
pub(super) fn reset(cfg: &Config) -> io::Result<()> {
    let held = cfg.jobs.join(HELD);
    let adopted = held.join(ADOPTED);
    let pending = held.join(PENDING);
    match fs::read(&pending) {
        Ok(bytes) => {
            if adopted.try_exists()? {
                fs::remove_file(&pending)?;
            } else {
                let mut adoption: Adoption = serde_json::from_slice(&bytes)?;
                adoption.refresh = true;
                adoption.save(&pending)?;
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match fs::remove_file(adopted) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Shared by discovery, completion and watchdog-disabled marker reset.
pub(super) fn lock(cfg: &Config) -> io::Result<crate::held::Held> {
    crate::held::Held::lock(
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(cfg.jobs.join(".adopt.lock"))?,
    )
}

/// Every job [`unheld`] finds, and whether every read it needed answered: a
/// directory, an entry or a file that couldn't be read is not a job that
/// isn't there. The saved policy judges every job at the same generation
/// and instant, including jobs that earlier scans could not read.
fn adoptable(
    cfg: &Config,
    policies: &[Option<burrow_settings::Snapshot>],
) -> (std::collections::BTreeSet<String>, bool) {
    let mut found = std::collections::BTreeSet::new();
    let mut complete = true;
    let Ok(entries) = fs::read_dir(&cfg.jobs) else {
        return (found, false);
    };
    for e in entries {
        let Ok(e) = e else {
            complete = false;
            continue;
        };
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        match e.path().metadata() {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(_) => {
                complete = false;
                continue;
            }
        }
        match unheld(cfg, &name, policies) {
            Ok(true) => {
                found.insert(name);
            }
            Ok(false) => {}
            Err(_) => complete = false,
        }
    }
    (found, complete)
}

/// A file's text through the confined reader, None when it isn't there; an
/// error for any other failure, a link or special file included.
fn present(path: &Path) -> io::Result<Option<String>> {
    match crate::state_reads::bytes(path) {
        Ok(b) => Ok(Some(
            String::from_utf8_lossy(&b)
                .trim_end_matches('\n')
                .to_owned(),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A job the policy denies, not parked (a park holds itself), with a wait
/// it keeps from the watchdog that [`live`] doesn't see: a turn that ended
/// other than done, or done without saying what comes next. Its own files
/// tell that, so a settled job costs no look at systemd ([`job_wait`] has
/// the last word on the rest). An error when a file it needs can't be read.
fn unheld(
    cfg: &Config,
    id: &str,
    policies: &[Option<burrow_settings::Snapshot>],
) -> io::Result<bool> {
    let job = Job::new(cfg, id);
    let Some(n) = present(&job.path("turns"))?.filter(|n| crate::job::valid_turn(n)) else {
        return Ok(false);
    };
    let outcome = present(&job.turn(&n, "outcome"))?;
    let end = present(&job.turn(&n, "end"))?;
    let open = outcome.as_deref() != Some("done") || end.as_deref() == Some("undeclared");
    Ok(open
        && !job.turn(&n, "stopped").try_exists()?
        && !job.path("owned").try_exists()?
        && !crate::policy::path(&job, "park").try_exists()?
        && policies.iter().any(|policy| {
            crate::policy::denied_by(&job, |repo| {
                policy
                    .as_ref()
                    .map(|s| s.decision(repo))
                    .ok_or_else(String::new)
            })
        })
        && job
            .facts()
            .is_some_and(|f| !f.stopped && job_wait(cfg, &f).is_some()))
}

#[cfg(test)]
mod tests;
