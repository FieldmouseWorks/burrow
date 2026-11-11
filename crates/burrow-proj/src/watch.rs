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
use crate::job::{Job, USAGE_LIMIT, awaits};
use crate::turn::start;
use crate::{Fail, filled, io_fail, now, read, swap, usage};

// Usage awaits from old Claude turns become its global hold. Codex turns
// keep the same wire format but wait independently until the reset.

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
    'jobs: for id in job_ids(cfg) {
        let job = Job::new(cfg, &id);
        let paused = crate::inbox::job_paused(cfg, &id);
        {
            let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
            if let Err(e) = crate::question::recover(&job) {
                eprintln!("proj question recovery: {e}");
                retry.push((String::new(), now() + RETRY));
                continue 'jobs;
            }
            if !paused {
                match crate::ask::expire(cfg, &job) {
                    Ok(Some(at)) => retry.push((String::new(), at)),
                    Ok(None) => (),
                    Err(e) => {
                        eprintln!("proj question: {e}");
                        retry.push((String::new(), now() + RETRY));
                    }
                }
            }
        }
        if let Err(error) = crate::ask::visibility(cfg, &job) {
            eprintln!("proj question: {error}");
            retry.push((String::new(), now() + RETRY));
        }
        // Notifications are independent of GitHub waits and include stopped
        // and completed jobs. Serialize with starts so rollback stays atomic.
        {
            let held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
            match crate::question::notify(cfg, &job) {
                Ok(false) => (),
                Ok(true) => retry.push((String::new(), now() + RETRY)),
                Err(e) => {
                    eprintln!("proj: {e}");
                    retry.push((String::new(), now() + RETRY));
                }
            }
            drop(held);
        }
        // Pausing stops automatic turns, not question reconciliation or
        // durable notification delivery (including stopped/completed jobs).
        if paused {
            if filled(&job.path("await")) || crate::ask::load(&job).is_some() {
                retry.push((String::new(), now() + RETRY));
            }
            continue;
        }
        if crate::ask::host_path(&job, "question-needs-owner").exists() {
            continue;
        }
        let f = job.path("await");
        if crate::planning::admit(&job).is_err() {
            continue;
        }
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
            if a.target == USAGE_LIMIT {
                if read(&job.turn(&turn, "engine")).as_deref() == Some("codex") {
                    let reset = a.was.parse::<i64>().unwrap_or(0);
                    if reset > now() {
                        lines.push_str(&a.line());
                        retry.push((String::new(), reset));
                    } else {
                        news.push_str(
                            "- Codex's usage limit has reset; carry on from the last turn\n",
                        );
                    }
                    continue;
                }
                let reset = a.was.parse::<i64>().unwrap_or(0);
                let written = a.since.parse::<i64>().ok();
                // Not taken: the job's waits stay as they are, for the next
                // look, which is booked.
                if let Err(e) = usage::earlier(cfg, &id, reset, written) {
                    eprintln!("{e}");
                    retry.push((String::new(), now() + RETRY));
                    continue 'jobs;
                }
                news.push_str("- the usage limit your last turn ended on: proj holds every turn in line until it resets, this one included\n");
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
                // By repo; "" (every repo's look) for one that's no repo's.
                for a in awaits(&waits) {
                    let repo = target(&a.target).map(|(r, _)| r).unwrap_or_default();
                    retry.push((repo, now() + RETRY));
                }
            }
        } else {
            swap(&f, &lines).map_err(|e| io_fail(&f, e))?;
        }
        drop(held);
    }
    // The owner's notice of the machine out of usage, tended here as well as
    // by the runs, which may never come: booked again, in every repo's look,
    // while there's anything left of it.
    if let Some(at) = usage::tend(cfg) {
        retry.push((String::new(), at));
    }
    book(cfg, only, &retry)
}

/// What a turn's end kicks for a wait on `target`: its repo's look, or
/// every repo's for one that's no repo's, such as an USAGE_LIMIT (which only
/// that takes in). burrow-events has no look for a repo by any other name.
pub fn kick_for(target: &str) -> String {
    match crate::github::target(target) {
        Some((repo, _)) => format!("proj-watch/{repo}"),
        None => "proj-watch".to_owned(),
    }
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
            if a.target == USAGE_LIMIT {
                if let Ok(due) = a.was.parse::<i64>()
                    && due > now
                {
                    add("", due);
                }
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::{USAGE_LIMIT, kick_for};

    // A wait on a PR or issue kicks its repo's look, however it's written; one
    // that's no repo's (an earlier proj's usage limit) kicks every repo's,
    // the only look that takes it in: burrow-events has none by its name.
    #[test]
    fn a_turns_end_kicks_the_look_that_takes_its_wait() {
        for (t, kick) in [
            ("sam/r#7", "proj-watch/sam/r"),
            ("https://github.com/sam/r/pull/7", "proj-watch/sam/r"),
            (
                "https://github.com/sam/r/issues/7#issuecomment-1",
                "proj-watch/sam/r",
            ),
            (USAGE_LIMIT, "proj-watch"),
            ("not a target", "proj-watch"),
        ] {
            assert_eq!(kick_for(t), kick, "{t}");
        }
    }
}

#[cfg(test)]
mod codex_tests {
    use super::*;
    #[test]
    fn a_codex_limit_is_booked_then_resumed_without_holding_claude() {
        let d = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = d.path().join("jobs");
        cfg.wake = d.path().join("wake.json");
        cfg.waits = None;
        cfg.kick.clear();
        cfg.tools.systemctl = "false".into();
        cfg.tools.systemd_run = "true".into();
        let job = Job::new(&cfg, "code");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(
            job.turn("1", "outcome"),
            "usage limit; waiting for Codex's reset",
        )
        .unwrap();
        fs::write(job.turn("1", "engine"), "codex").unwrap();
        let due = now() + 10000;
        let await_text = format!("usage-limit\t{}\t{due}\n", now());
        fs::write(job.path("await"), &await_text).unwrap();
        look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert_eq!(fs::read_to_string(job.path("await")).unwrap(), await_text);
        let wake: Value = serde_json::from_slice(&fs::read(&cfg.wake).unwrap()).unwrap();
        assert_eq!(wake[""], due);
        assert!(crate::usage::kept(&cfg).is_none());
        fs::write(
            job.path("await"),
            format!("usage-limit\t1000\t{}\n", now() - 1),
        )
        .unwrap();
        // A due wake from an inbox job remains parked while its repo is
        // paused, then joins the ordinary turn queue when enabled again.
        drop(job);
        let inbox = cfg.inbox.as_mut().unwrap();
        inbox.state = d.path().join("inbox");
        let record = inbox.state.join("sam_r-7");
        fs::create_dir_all(&record).unwrap();
        fs::write(record.join("job"), "code").unwrap();
        fs::write(record.join("drained"), "sam/r#7").unwrap();
        cfg.paused = vec!["SAM/R".into()];
        let job = Job::new(&cfg, "code");
        let parked = fs::read_to_string(job.path("await")).unwrap();
        look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert_eq!(fs::read_to_string(job.path("await")).unwrap(), parked);
        let wake: Value = serde_json::from_slice(&fs::read(&cfg.wake).unwrap()).unwrap();
        assert!(wake[""].as_i64().unwrap() > now());
        drop(job);
        cfg.paused.clear();
        let job = Job::new(&cfg, "code");
        look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(
            fs::read_to_string(job.path("prompt.2"))
                .unwrap()
                .contains("Codex's usage limit has reset")
        );
        assert!(crate::usage::kept(&cfg).is_none());
    }
}
