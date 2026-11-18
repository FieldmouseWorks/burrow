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
use crate::github::{changes, fingerprint, over, target};
use crate::job::{Job, USAGE_LIMIT, awaits};
use crate::record::JobFile;
use crate::turn::{start, start_closed};
use crate::{Fail, io_fail, now, swap, usage};

// Usage awaits from old turns become Claude's global hold.

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
    crate::waits::rewatch_all(cfg);
    let hours = cfg.await_hours as i64;
    let mut retry: Vec<(String, i64)> = Vec::new();
    'jobs: for id in job_ids(cfg) {
        let job = Job::new(cfg, &id);
        if job
            .turns()
            .is_some_and(|n| crate::record::transferring(&job, &n))
        {
            continue;
        }

        let stopped = job
            .turns()
            .is_some_and(|n| crate::record::stopped(&job, &n));
        let paused = crate::policy::parked(&job) || crate::inbox::job_paused(cfg, &id);
        {
            let _held = job
                .lock()
                .map_err(|e| io_fail(&job.file(JobFile::Lock), e))?;
            if let Err(e) = crate::question::recover(&job) {
                eprintln!("proj question recovery: {e}");
                retry.push((String::new(), now() + RETRY));
                continue 'jobs;
            }
            if !paused && !stopped {
                // Commit recovered asks and one-way upgrades before an old
                // reversible deadline can execute, and before visibility and
                // durable notification processing below.
                if crate::planning::admit(&job).is_ok() {
                    match crate::github_quota::reconcile(&job) {
                        Ok(Some(due)) if due > now() => retry.push((String::new(), due)),
                        Ok(_) => (),
                        Err(e) => {
                            eprintln!("proj: quota recovery: {:?}", e.say);
                            retry.push((String::new(), now() + RETRY));
                            continue 'jobs;
                        }
                    }
                }
                match crate::ask::expire(cfg, &job) {
                    Ok(Some(at)) => retry.push((String::new(), at)),
                    Ok(None) => (),
                    Err(e) => {
                        eprintln!("proj question: {e}");
                        if !job
                            .turns()
                            .is_some_and(|n| crate::record::stopped(&job, &n))
                        {
                            retry.push((String::new(), now() + RETRY));
                        }
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
            let held = job
                .lock()
                .map_err(|e| io_fail(&job.file(JobFile::Lock), e))?;
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
        if stopped {
            continue;
        }
        // Pausing stops automatic turns, not question reconciliation or
        // durable notification delivery (including stopped/completed jobs).
        if paused {
            if crate::record::awaiting(&job)
                || job.file(JobFile::GithubPending).exists()
                || !matches!(crate::ask::load(&job), Ok(None))
            {
                retry.push((String::new(), now() + RETRY));
            }
            continue;
        }
        if crate::ask::host_path(&job, "question-needs-owner").exists() {
            continue;
        }
        if crate::planning::admit(&job).is_err() {
            continue;
        }
        if !crate::record::awaiting(&job) || matches!(job.state().as_str(), "queued" | "running") {
            continue;
        }
        let Some(turn) = job.turns() else { continue };
        // Gone already: another look (burrow-events', or a hand-run one) has
        // started its next turn.
        let Some(waits) = crate::record::await_text(&job) else {
            continue;
        };
        let (mut news, mut lines) = (String::new(), String::new());
        // The first awaited PR or issue this look sees close: it settles a
        // reversible open question (question::Answer::Closed).
        let mut closed = None;
        for a in awaits(&waits) {
            if a.target.is_empty() {
                continue;
            }
            if a.target == crate::github_quota::LIMIT {
                let Some((_, reset)) = crate::github_quota::reset(&job, &turn) else {
                    continue;
                };
                if reset > now() {
                    lines.push_str(&a.line());
                    retry.push((String::new(), reset));
                } else if job.file(JobFile::GithubPending).exists()
                    || match crate::github_quota::captured(&job) {
                        Ok(text) => !text.is_empty(),
                        Err(_) => true,
                    }
                {
                    let held = job
                        .lock()
                        .map_err(|e| io_fail(&job.file(JobFile::Lock), e))?;
                    if job.turns().as_deref() != Some(turn.as_str())
                        || crate::record::await_text(&job).as_deref() != Some(waits.as_str())
                    {
                        drop(held);
                        continue 'jobs;
                    }
                    let replayed = if job.file(JobFile::GithubPending).exists() {
                        crate::github_quota::replay(cfg, &job)
                    } else {
                        crate::github_quota::restore(&job)
                            .map(|()| None)
                            .map_err(crate::github_quota::RecoveryError::State)
                    };
                    drop(held);
                    match replayed {
                        Ok(due) => {
                            retry.push((String::new(), due.unwrap_or_else(|| now() + 1)));
                            crate::waits::rewatch(cfg, &[&id]);
                            continue 'jobs;
                        }
                        Err(crate::github_quota::RecoveryError::Request(e)) => {
                            news.push_str(&format!(
                                "- GitHub reset, but your saved await failed: {}\n",
                                e.say.unwrap_or_else(|| format!("exit {}", e.code))
                            ))
                        }
                        Err(crate::github_quota::RecoveryError::State(e)) => {
                            eprintln!("proj: quota recovery state unavailable: {:?}", e.say);
                            retry.push((String::new(), now() + RETRY));
                            continue 'jobs;
                        }
                    }
                } else {
                    news.push_str("- GitHub quota has reset; carry on from the last turn\n");
                }
                continue;
            }
            if a.target == USAGE_LIMIT {
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
                (Some(fp @ Value::Object(now)), Some(Some(old @ Value::Object(was))))
                    if now != was =>
                {
                    news.push_str(&format!("- {t}: {}\n", changes(was, now)));
                    if let (Some(how), None, None) = (over(fp), over(old), &closed) {
                        closed = Some(format!("{t} was {how}"));
                    }
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
        let held = job
            .lock()
            .map_err(|e| io_fail(&job.file(JobFile::Lock), e))?;
        if job.turns().as_deref() != Some(turn.as_str())
            || crate::record::await_text(&job).as_deref() != Some(waits.as_str())
        {
        } else if !news.is_empty() {
            // The new turn clears what's left: if it still waits, it says so
            // again. One that can't start keeps its waits, and is tried again
            // in five minutes.
            let prompt = format!(
                "News on what you were waiting on:\n\n{news}\nCarry on from there. Anything you left in the background last turn was stopped when it ended."
            );
            let started = match closed {
                Some(what) => start_closed(cfg, &id, &prompt, what),
                None => start(cfg, &id, &prompt, false),
            };
            if let Err(e) = started {
                eprintln!("{e}");
                if job
                    .turns()
                    .is_some_and(|n| crate::record::stopped(&job, &n))
                {
                    continue 'jobs;
                }
                // By repo; "" (every repo's look) for one that's no repo's.
                for a in awaits(&waits) {
                    let repo = target(&a.target).map(|(r, _)| r).unwrap_or_default();
                    retry.push((repo, now() + RETRY));
                }
            }
        } else {
            let mark = crate::record::mark(&job, crate::record::Marker::Await);
            mark.swap(&lines).map_err(|e| mark.fail(e))?;
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
        let job = Job::new(cfg, &id);
        if crate::policy::parked(&job) || crate::policy::denied(&job) {
            continue;
        }
        let turn = job.turns().unwrap_or_default();
        if crate::record::stopped(&job, &turn) {
            continue;
        }
        let text = crate::record::await_text(&job).unwrap_or_default();
        if let Some((_, deadline)) = crate::github_quota::reset(&job, &turn) {
            add(
                "",
                if deadline > now {
                    deadline
                } else {
                    now + RETRY
                },
            );
        }
        for a in awaits(&text) {
            let Some(since) = (!a.since.is_empty() && a.since.bytes().all(|b| b.is_ascii_digit()))
                .then(|| a.since.parse::<i64>().ok())
                .flatten()
            else {
                continue;
            };
            if a.target == crate::github_quota::LIMIT {
                continue;
            }
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
