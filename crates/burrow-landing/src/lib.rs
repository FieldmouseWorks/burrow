//! burrow-landing: who lands each repository's PRs, the steward or conaryd,
//! moved as `settings landing OWNER/REPO steward|conaryd` says (Conary#313).
//!
//! It alone writes the record (burrow_settings::landing), and conaryd's
//! events file, whose `repos` are the ones the record gives conaryd. A repo
//! changes hands in two steps, so the two never land in it at once:
//!
//! 1. The handover starts: the record names who it goes to (`to`). From
//!    then on neither owner lands there. The steward reads that in its
//!    `decision` and lets go: it stops its jobs and proof builds in the repo
//!    and, once none is left, says so in its release marker, naming the
//!    handover's generation. conaryd lets go by being taken off its events
//!    file: it passes no repo it isn't given.
//! 2. Once the one letting go is quiet, the record gives the repo to the
//!    other. The steward is quiet when its marker names this handover;
//!    conaryd when no pass of the repo holds the flow's lock and every
//!    worker it launched there has exited (Conary's own lock and its
//!    workers' `.running` locks, waited on, never guessed).
//!
//! Then the new owner is woken: conaryd is given the repo in its events
//! file, whose change restarts it (modules/landing.nix), and its start kicks
//! every repo it's given; the steward gets a kick of the repo's pass.
//! Setting the owner back before a handover is done ends it where it is,
//! and the owner that was letting go carries on. Each run reads everything
//! again, so one cut short (a restart, a crash) goes on from the record.

use burrow_settings::landing::{Holding, Owner, Record};
use conary_github::events::Runtime;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// burrow-settings' tracked file: the overlay, the record's path and
    /// the repos conaryd may have.
    pub settings: PathBuf,
    /// The steward's release marker: repo -> the generation it let go at.
    pub steward_released: PathBuf,
    /// conaryd's events file as burrow renders it, `repos` empty: checked
    /// against Conary's types as the machine is built.
    pub events_template: PathBuf,
    /// The events file conaryd reads, the template with its repos.
    pub events: PathBuf,
    /// Where a kick of the steward's pass of a repo goes, `/OWNER/REPO`
    /// added: burrow-events' kick of the steward's handler.
    pub steward_kick: String,
}

/// What a run does beyond its files: wake the owner a repo went to.
pub trait Wake {
    fn steward(&mut self, repo: &str) -> Result<(), String>;
}

/// burrow-events' kick port.
pub struct Kick<'a>(pub &'a str);

impl Wake for Kick<'_> {
    fn steward(&mut self, repo: &str) -> Result<(), String> {
        ureq::post(format!("{}/{repo}", self.0))
            .send_empty()
            .map(drop)
            .map_err(|e| format!("kicking the steward's pass of {repo}: {e}"))
    }
}

/// How long a run waits for conaryd to let go of a repo: its pass to end
/// and each of its workers to exit. A worker's own deadline is the job or
/// proof timeout, so the unit's timeout bounds this, not the code.
pub struct Patience {
    pub lock: Duration,
}

/// What one run changed, for its log and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Handovers that started: repo -> who it goes to.
    pub started: BTreeMap<String, Owner>,
    /// Handovers set back before they were done: the owner carries on.
    pub undone: BTreeSet<String>,
    /// Repos that changed hands: repo -> their new owner.
    pub moved: BTreeMap<String, Owner>,
    /// Repos still waiting on the one letting go.
    pub waiting: BTreeSet<String>,
}

/// One run: every repo whose configured owner differs from the record's
/// starts or ends its handover, and every handover whose letting-go side is
/// quiet completes. Fails, changing nothing, when the settings, the record,
/// the marker or the template can't be read: nobody's landing moves on a
/// guess.
pub fn run(cfg: &Config, wake: &mut dyn Wake, patience: &Patience) -> Result<Moved, String> {
    let settings: burrow_settings::Config = read_json(&cfg.settings)?;
    let path = settings
        .landing_record
        .clone()
        .ok_or("burrow-settings keeps no landing record (landingRecord)")?;
    let configured = settings.landing_owners()?;
    let mut record = Record::read(&path)?;
    let template: Runtime = read_json(&cfg.events_template)?;
    let mut moved = Moved::default();
    let now = burrow_settings::now();

    // Step 1: start or undo each handover the setting asks for.
    let repos: BTreeSet<String> = configured
        .keys()
        .chain(record.repos.keys())
        .cloned()
        .collect();
    for repo in &repos {
        let want = configured.get(repo).copied().unwrap_or_default();
        let h = record.holding(repo);
        let next = match h.to {
            None if h.owner != want => {
                moved.started.insert(repo.clone(), want);
                Holding {
                    to: Some(want),
                    generation: h.generation + 1,
                    since: now,
                    ..h
                }
            }
            Some(_) if h.owner == want => {
                moved.undone.insert(repo.clone());
                Holding {
                    to: None,
                    generation: h.generation + 1,
                    since: now,
                    ..h
                }
            }
            _ => continue,
        };
        record.repos.insert(repo.clone(), next);
    }
    // Published before anything waits on it: from here the steward lets go
    // at its next decision, and conaryd is no longer given a repo it's
    // handing back.
    publish(&record, &path, &template, &cfg.events)?;
    for repo in moved.started.keys().chain(&moved.undone) {
        if record.holding(repo).owner == Owner::Steward {
            // It lets go, or carries on, at its next pass: this one.
            wake.steward(repo)?;
        }
    }

    // Step 2: each handover whose letting-go side is quiet completes.
    let released: BTreeMap<String, u64> = match std::fs::read(&cfg.steward_released) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        _ => read_json(&cfg.steward_released)?,
    };
    for (repo, h) in record.repos.clone() {
        let Some(to) = h.to else { continue };
        let quiet = match h.owner {
            Owner::Steward => released.get(&repo) == Some(&h.generation),
            Owner::Conaryd => conaryd_quiet(&template.data, &repo, patience)?,
        };
        if !quiet {
            moved.waiting.insert(repo);
            continue;
        }
        record.repos.insert(
            repo.clone(),
            Holding {
                owner: to,
                to: None,
                since: burrow_settings::now(),
                ..h
            },
        );
        moved.moved.insert(repo, to);
    }
    if !moved.moved.is_empty() {
        publish(&record, &path, &template, &cfg.events)?;
    }
    for (repo, to) in &moved.moved {
        if *to == Owner::Steward {
            wake.steward(repo)?;
        }
    }
    Ok(moved)
}

/// The record, then conaryd's events file from it: the repos the record
/// gives conaryd, and no other. Written only when it changes, since a
/// change restarts conaryd.
fn publish(record: &Record, path: &Path, template: &Runtime, events: &Path) -> Result<(), String> {
    if Record::read(path).ok().as_ref() != Some(record) {
        record.write(path)?;
    }
    let mut runtime = template.clone();
    runtime.repos = record
        .repos
        .iter()
        .filter(|(_, h)| h.lands(Owner::Conaryd))
        .map(|(repo, _)| {
            repo.parse()
                .map_err(|e| format!("{repo}: not a repository: {e}"))
        })
        .collect::<Result<_, String>>()?;
    let mut body = serde_json::to_vec_pretty(&runtime).map_err(|e| e.to_string())?;
    body.push(b'\n');
    if std::fs::read(events).ok().as_deref() == Some(&body[..]) {
        return Ok(());
    }
    let pending = events.with_extension("pending");
    std::fs::write(&pending, &body)
        .and_then(|()| std::fs::File::open(&pending)?.sync_all())
        .and_then(|()| std::fs::rename(&pending, events))
        .map_err(|e| format!("{}: {e}", events.display()))
}

/// Whether conaryd has let go of `repo`, which it's no longer given: no
/// pass of it holds the flow's repository lock, and every worker it
/// launched there has exited. Waits for both: a pass for at most
/// `patience.lock`, a worker until it exits.
fn conaryd_quiet(data: &Path, repo: &str, patience: &Patience) -> Result<bool, String> {
    let name: conary_core::op::Repo = repo
        .parse()
        .map_err(|e| format!("{repo}: not a repository: {e}"))?;
    // A repo conaryd never passed has no state to wait on, and opening it
    // would make its directory.
    if !data.join("flow").is_dir() {
        return Ok(true);
    }
    let started = std::time::Instant::now();
    let dir = loop {
        match conary_github::flow::Data::open(data, &name) {
            // Held now, no pass of it can be running; dropped at once, so
            // nothing of conaryd's waits on this run.
            Ok(held) => {
                break held
                    .path(0)
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or("the flow's directory has no parent")?;
            }
            Err(e) if started.elapsed() >= patience.lock => {
                return Err(format!(
                    "{repo}: conaryd's pass of it didn't end within {}s: {e}",
                    patience.lock.as_secs()
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let outstanding = conary_github::flow::jobs::outstanding(&dir).map_err(|e| e.to_string())?;
    let (ended, gone) = std::sync::mpsc::channel();
    for job in &outstanding {
        let ended = ended.clone();
        conary_github::flow::jobs::watch(
            job,
            std::sync::Arc::new(move || {
                let _ = ended.send(());
            }),
        )
        .map_err(|e| format!("{}: {e}", job.display()))?;
    }
    for _ in &outstanding {
        gone.recv().map_err(|e| e.to_string())?;
    }
    Ok(true)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests;
