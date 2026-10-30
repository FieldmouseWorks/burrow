//! proj-watch's look (`proj _watch [OWNER/REPO]`): jobs waiting on GitHub
//! whose PRs or issues (in that repo, or any) changed get their next turn.
//! One look at a time, whoever started it (burrow-events, from before a
//! restart too, or a hand): two would each write the wake file from what
//! they saw.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use serde_json::Value;

use crate::config::Config;
use crate::github::{changes, fingerprint, target};
use crate::job::{Job, awaits};
use crate::turn::start;
use crate::{Fail, filled, io_fail, now, read, swap};

/// Seconds before a repo GitHub couldn't be asked about, or a turn that
/// couldn't start, is looked at again.
const RETRY: i64 = 300;

pub fn look(cfg: &Config, only: &str) -> Result<(), Fail> {
    let lock = PathBuf::from(format!("{}.lock", cfg.wake.display()));
    let pass = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&lock)
        .map_err(|e| io_fail(&lock, e))?;
    pass.lock().map_err(|e| io_fail(&lock, e))?;
    let hours = cfg.await_hours as i64;
    let mut retry: Vec<(String, i64)> = Vec::new();
    for id in job_ids(cfg) {
        let job = Job::new(cfg, &id);
        let f = job.path("await");
        if !filled(&f) || matches!(job.state().as_str(), "queued" | "running") {
            continue;
        }
        let Some(turn) = job.turns() else { continue };
        // Gone already: another look (burrow-events', or a hand-run one) has
        // started its next turn.
        let Some(waits) = read(&f) else { continue };
        let (mut news, mut lines) = (String::new(), String::new());
        for a in awaits(&waits) {
            if a.target.is_empty() {
                continue;
            }
            let was = (!a.was.is_empty()).then(|| serde_json::from_str::<Value>(&a.was).ok());
            let since = (!a.since.is_empty() && a.since.bytes().all(|b| b.is_ascii_digit()))
                .then(|| a.since.parse::<i64>().ok())
                .flatten();
            let (Some((repo, n)), Some(since), None | Some(Some(Value::Object(_)))) =
                (target(&a.target), since, &was)
            else {
                // Not a line proj await writes (one edited by hand): the job
                // hears so rather than waiting on it for good.
                news.push_str(&format!(
                    "- {}: not something proj can watch; no longer watched\n",
                    a.target
                ));
                continue;
            };
            // GitHub's names are case-blind: the await's spelling needn't be
            // the event's.
            if !only.is_empty() && repo.to_lowercase() != only.to_lowercase() {
                lines.push_str(&a.line());
                continue;
            }
            // GitHub can't be asked just now: looked at again in five minutes.
            let fp = fingerprint(cfg, &repo, &n);
            if fp.is_none() {
                retry.push((repo.clone(), now() + RETRY));
            }
            let expired = now() - since >= hours * 3600;
            let t = &a.target;
            match (&fp, &was) {
                (Some(Value::Object(now)), Some(Some(Value::Object(was)))) if now != was => {
                    news.push_str(&format!("- {t}: {}\n", changes(was, now)));
                }
                (Some(_), _) if expired => {
                    news.push_str(&format!("- {t}: nothing changed in {hours}h; no longer watched\n"));
                }
                // The deadline holds even when GitHub can't be read (down,
                // or the repo gone): the job hears that rather than waiting
                // for good.
                (None, _) if expired => news.push_str(&format!(
                    "- {t}: couldn't be read from GitHub, and {hours}h have passed; no longer watched\n"
                )),
                _ => {
                    let baseline = match (&was, &fp) {
                        (Some(_), _) => a.was.clone(),
                        (None, Some(fp)) => fp.to_string(),
                        (None, None) => String::new(),
                    };
                    lines.push_str(&format!("{t}\t{}\t{baseline}\n", a.since));
                }
            }
        }
        // GitHub was asked without the lock, so a reply may have started a
        // turn meanwhile (that turn owns the job's waits), or proj stop ended
        // the job: either way these are dropped.
        let held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
        if job.turns().as_deref() != Some(turn.as_str())
            || read(&f).as_deref() != Some(waits.as_str())
        {
        } else if !news.is_empty() {
            // The new turn clears what's left: if it still waits, it says so
            // again. One that can't start keeps its waits, and is tried again
            // in five minutes.
            let prompt = format!(
                "News on what you were waiting on:\n\n{news}\nCarry on from there. Anything you left in the background last turn was stopped when it ended."
            );
            if let Err(e) = start(cfg, &id, &prompt, false) {
                eprintln!("{e}");
                for a in awaits(&waits) {
                    let repo = a.target.split('#').next().unwrap_or_default().to_owned();
                    retry.push((repo, now() + RETRY));
                }
            }
        } else {
            swap(&f, &lines).map_err(|e| io_fail(&f, e))?;
        }
        drop(held);
    }
    book(cfg, only, &retry)
}

/// The jobs, by id: every folder in jobs but the dot ones.
fn job_ids(cfg: &Config) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(&cfg.jobs)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// When each repo needs a look next, for burrow-events: the deadline of
/// every job's waits that's still to come, and the retries. One already past
/// is a turn that couldn't start (its retry stands for it) or one this look
/// skipped as running, whose end kicks proj-watch itself. A look at one repo
/// keeps the other repos' wake-ups (their retries too).
fn book(cfg: &Config, only: &str, retry: &[(String, i64)]) -> Result<(), Fail> {
    let now = now();
    let mut at: BTreeMap<String, i64> = BTreeMap::new();
    let mut add = |repo: &str, when: i64| {
        let e = at.entry(repo.to_owned()).or_insert(when);
        *e = (*e).min(when);
    };
    if !only.is_empty()
        && let Some(Value::Object(booked)) = fs::read(&cfg.wake)
            .ok()
            .and_then(|t| serde_json::from_slice(&t).ok())
    {
        for (repo, when) in booked {
            let when = when
                .as_i64()
                .or_else(|| when.as_f64().map(|w| w.ceil() as i64));
            if let Some(when) = when
                && repo.to_lowercase() != only.to_lowercase()
                && when > now
            {
                add(&repo, when);
            }
        }
    }
    let hours = cfg.await_hours as i64;
    for id in job_ids(cfg) {
        for a in awaits(&read(&Job::new(cfg, &id).path("await")).unwrap_or_default()) {
            let Some(since) = (!a.since.is_empty() && a.since.bytes().all(|b| b.is_ascii_digit()))
                .then(|| a.since.parse::<i64>().ok())
                .flatten()
            else {
                continue;
            };
            let due = since + hours * 3600 + 1;
            if let Some((repo, _)) = a.target.split_once('#')
                && !repo.contains('#')
                && !a.target[repo.len() + 1..].contains('#')
                && due > now
            {
                add(repo, due);
            }
        }
    }
    for (repo, when) in retry {
        add(repo, *when);
    }
    let text = serde_json::to_string_pretty(&at).map_err(|e| Fail::new(1, format!("proj: {e}")))?;
    swap(&cfg.wake, &format!("{text}\n")).map_err(|e| io_fail(&cfg.wake, e))
}
