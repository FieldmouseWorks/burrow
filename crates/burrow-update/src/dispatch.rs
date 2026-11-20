//! A problem: handed to a Claude job, told to the owner when they must know
//! (HOST-notify), and settled once a later run finds it fixed.
//!
//! It goes to a Claude job in the machine's own project, one per problem
//! and what it's about (the inputs that don't build) while that job is at
//! work, however long it waits for a slot: proj keeps it under its key
//! (`proj run --key KEY --join`) and a run that finds it at work joins it.
//! A problem whose job has ended gets another, 6 hours after the last run
//! handed it on at the soonest.
//! Fixing what deploys everything is work on the pipeline itself (proj run
//! --pipeline): it goes ahead of everyone's jobs, in the slot kept for it,
//! never behind a project's long ones. proj counts the machine's flake as
//! the pipeline's own (proj's pick), with or without autoland's
//! pipelineRepos. The job acts on GitHub as the machine's App, when it has
//! one.

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::Config;
use crate::status::epoch;

/// How long after a run last handed a problem on before one whose job has
/// ended gets another: a job that gave up isn't started again each run.
const RECENT: Duration = Duration::from_secs(6 * 3600);

/// The most of a proj key (burrow-proj's `key_name`).
const KEY_MAX: usize = 200;

/// `key`'s problem about `of` (none for the problem as a whole), said in
/// `prompt`, handed to a job: its id, the one at work on it already when
/// there is one, or nothing when a run handed it on recently or proj didn't
/// take it (proj says why, in the journal). jobs/KEY keeps the id.
pub fn dispatch(cfg: &Config, key: &str, of: &[String], prompt: &str) -> Option<String> {
    let f = cfg.state.join("jobs").join(key);
    if let Ok(meta) = fs::metadata(&f) {
        // A time still to come, as a clock set back leaves one, is recent.
        let recent = match meta.modified().map(|m| SystemTime::now().duration_since(m)) {
            Ok(Ok(age)) => age < RECENT,
            Ok(Err(_)) => true,
            Err(_) => false,
        };
        if meta.len() > 0 && recent {
            return None;
        }
    }
    let mut c = cfg.run_as_user("proj");
    c.args(["run", "--pipeline", "--role", "update"]);
    c.args(["--key", &job_key(cfg, key, of), "--join"]);
    if let Some(app) = &cfg.app {
        c.args(["--app", app]);
    }
    let out = c
        .args([&cfg.project, prompt])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let id = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string();
    if let Err(e) = fs::write(&f, format!("{id}\n")) {
        eprintln!("{}: jobs/{key}: {e}", cfg.update);
    }
    Some(id)
}

/// What proj keeps `key`'s problem about `of` under: UPDATE/KEY, then the
/// inputs, sorted, so the same failure is the same job whichever run sees
/// it. Inputs too many for a key, or named past what one takes, are their
/// hash.
pub fn job_key(cfg: &Config, key: &str, of: &[String]) -> String {
    let problem = format!("{}/{key}", cfg.update);
    if of.is_empty() {
        return problem;
    }
    let mut of = of.to_vec();
    of.sort();
    of.dedup();
    let of = of.join(":");
    let whole = format!("{problem}/{of}");
    let named = of
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
    if named && whole.len() <= KEY_MAX {
        return whole;
    }
    // FNV-1a: the same on every run, unlike std's hasher.
    let hash = of.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{problem}/{hash:016x}")
}

/// The owner hears of `key`'s problem: `title`, and `body` for what it is
/// and who's on it. HOST-notify keeps one notice a problem, and says
/// whatever GitHub does, in the journal.
pub fn open(cfg: &Config, key: &str, title: &str, body: &str) {
    notify(cfg, &["open", key, title, body]);
}

/// `key`'s problem, settled: its job and how long it's been seen forgotten,
/// and its notice, if it had one, closed with `why`.
pub fn settled(cfg: &Config, key: &str, why: &str) {
    let _ = fs::remove_file(cfg.state.join("jobs").join(key));
    let _ = fs::remove_file(cfg.state.join(format!("since-{key}")));
    notify(cfg, &["close", key, why]);
}

/// How long, in seconds, `key`'s problem has been seen, unsettled, from the
/// run that first saw it (since-KEY).
pub fn age(cfg: &Config, key: &str) -> i64 {
    let since = cfg.state.join(format!("since-{key}"));
    if !since.exists() {
        let _ = fs::write(&since, format!("{}\n", epoch()));
    }
    epoch()
        - cfg
            .recorded(&format!("since-{key}"))
            .parse::<i64>()
            .unwrap_or(epoch())
}

fn notify(cfg: &Config, words: &[&str]) {
    match Command::new(&cfg.tools.notify).args(words).status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!(
            "{}: {} {}: {s}",
            cfg.update,
            cfg.tools.notify.display(),
            words[0]
        ),
        Err(e) => eprintln!("{}: {}: {e}", cfg.update, cfg.tools.notify.display()),
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
