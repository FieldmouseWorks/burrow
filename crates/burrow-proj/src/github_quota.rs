//! GitHub quota holds and durable declarations. A rate limit is a scheduler
//! deadline; neither the engine nor the owner needs a correction turn for it.
use crate::state_reads::write as swap;
use crate::{config::Config, job::Job, now, read};
use burrow_github::quota::Meter;
use std::{collections::BTreeMap, fs};
pub(crate) const LIMIT: &str = "github-limit";
pub(crate) const PENDING: &str = "github-await";
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Pending {
    pub targets: Vec<String>,
    pub retry_at: u64,
}

pub fn meters(cfg: &Config) -> Vec<Meter> {
    let mut latest: BTreeMap<(String, String), Meter> = BTreeMap::new();
    for job in fs::read_dir(&cfg.jobs)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        for file in fs::read_dir(job.path().join("github"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            if file.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            if let Some(mut m) = Meter::read(&file.path()) {
                let key = (m.app.clone(), m.owner.clone());
                if let Some(old) = latest.get_mut(&key) {
                    let hold = old
                        .until(now().max(0) as u64)
                        .into_iter()
                        .chain(m.until(now().max(0) as u64))
                        .max();
                    if old.at <= m.at {
                        m.retry_at = hold;
                        *old = m;
                    } else {
                        old.retry_at = hold;
                    }
                } else {
                    latest.insert(key, m);
                }
            }
        }
    }
    latest.into_values().collect()
}

pub(crate) fn held(cfg: &Config, job: &Job, engine: crate::engine::Engine) -> Option<i64> {
    let app = crate::turn::github_app(cfg, read(&job.path("app")), engine)
        .ok()
        .flatten()?;
    let repo = read(&job.path("link"))
        .and_then(|link| crate::github::target(&link).map(|(r, _)| r))
        .unwrap_or_else(|| crate::pick::github_repo(cfg, &crate::pick::project(&job.dir)));
    let owner = repo.split('/').next()?;
    meters(cfg)
        .into_iter()
        .filter(|m| m.app == app && m.owner.eq_ignore_ascii_case(owner))
        .filter_map(|m| m.until(now().max(0) as u64))
        .max()
        .map(|t| t as i64)
}

pub(crate) fn note(cfg: &Config, job: &Job, owner: &str, due: u64) -> Result<(), String> {
    let engine = read(&job.turn(&job.turns().unwrap_or_default(), "engine"))
        .as_deref()
        .and_then(|s| crate::engine::Engine::parse(s).ok())
        .unwrap_or(crate::engine::Engine::Claude);
    if let Ok(Some(app)) = crate::turn::github_app(cfg, read(&job.path("app")), engine) {
        let mut meter = Meter::new(&app, owner, now().max(0) as u64);
        meter.retry_at = Some(due);
        meter
            .record(&job.path("github"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Only actual failed API calls write refusal, never quoted tool results.
pub(crate) fn refused(job: &Job) -> Option<i64> {
    Meter::read(&job.path("github/refusal"))?
        .retry_at
        .map(|t| t as i64)
}

pub(crate) fn finish(job: &Job, n: &str) -> Result<bool, String> {
    if job.turn(n, "stopped").exists() {
        return Ok(false);
    }
    let Some(due) = read(&job.path(PENDING))
        .and_then(|s| serde_json::from_str::<Pending>(&s).ok())
        .map(|p| p.retry_at as i64)
        .or_else(|| refused(job))
    else {
        return Ok(false);
    };
    park(job, due)?;
    swap(job.turn(n, "result"), format!("GitHub quota is exhausted. This job is parked until {}; saved waits will be registered automatically.\n", crate::usage::when(due))).map_err(|e| e.to_string())?;
    swap(job.turn(n, "end"), "await\n").map_err(|e| e.to_string())?;
    swap(job.turn(n, "outcome"), "GitHub quota; waiting for reset\n").map_err(|e| e.to_string())?;
    Ok(true)
}

pub(crate) fn park(job: &Job, due: i64) -> Result<(), String> {
    swap(job.path("await"), format!("{LIMIT}\t{}\t{due}\n", now())).map_err(|e| e.to_string())?;
    crate::ask::clear(job);
    for name in ["end.next", "ask.next"] {
        let _ = fs::remove_file(job.path(name));
    }
    Ok(())
}

/// Recover an await after the reset, without asking the engine to repeat it.
/// The intent survives partial API failure and process/reboot boundaries.
pub(crate) fn replay(cfg: &Config, job: &Job) -> Result<Option<i64>, crate::Fail> {
    let Some(text) = read(&job.path(PENDING)) else {
        return Ok(None);
    };
    let mut pending: Pending =
        serde_json::from_str(&text).map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let targets = &pending.targets;
    let found = targets
        .iter()
        .map(|t| {
            crate::github::target(t)
                .ok_or_else(|| crate::Fail::new(1, "invalid saved GitHub target"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::github::take_limit();
    let waits = match crate::cmd::baselines(cfg, &found, targets, None) {
        Ok(waits) => waits,
        Err(error) => {
            if let Some(due) = crate::github::take_limit() {
                pending.retry_at = due;
                swap(
                    job.path(PENDING),
                    serde_json::to_vec(&pending).expect("pending await"),
                )
                .map_err(|e| crate::io_fail(&job.path(PENDING), e))?;
                park(job, due as i64).map_err(|e| crate::Fail::new(1, e))?;
                return Ok(Some(due as i64));
            }
            return Err(error);
        }
    };
    // Publish the complete wait before removing intent. Repeating after a
    // crash is safe: these are reads, never GitHub mutations.
    let mut kept = read(&job.path("await.next")).unwrap_or_default();
    kept.push_str(&waits);
    swap(job.path("await"), &kept).map_err(|e| crate::io_fail(&job.path("await"), e))?;
    if let Some(n) = job.turns() {
        swap(job.turn(&n, "end"), "await\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "end"), e))?;
        swap(job.turn(&n, "outcome"), "done\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "outcome"), e))?;
    }
    fs::remove_file(job.path(PENDING)).map_err(|e| crate::io_fail(&job.path(PENDING), e))?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, Config) {
        let d = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = d.path().join("jobs");
        cfg.wake = d.path().join("wake");
        cfg.inbox = None;
        cfg.waits = None;
        cfg.kick.clear();
        cfg.tools.systemctl = "false".into();
        cfg.tools.systemd_run = "true".into();
        let gh = d.path().join("gh");
        burrow_test::executable(
            &gh,
            format!(
                r#"#!/bin/sh
if [ -f '{}/limited' ]; then
  printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: {}\n\n{{"message":"API rate limit exceeded"}}'; exit 1
fi
if [ -f '{}/denied' ]; then
  printf 'HTTP/2.0 403 Forbidden\n\n{{"message":"Resource not accessible by integration"}}'; exit 1
fi
printf 'HTTP/2.0 200 OK\n\n'
case "$*" in */comments*) if [ -f "$(dirname "$0")/changed" ]; then echo '[{{"body":"news"}}]'; else echo '[]'; fi;; *) echo '{{"state":"open"}}';; esac
"#,
                d.path().display(),
                now() + 3600,
                d.path().display()
            ),
        );
        cfg.tools.gh = gh;
        (d, cfg)
    }
    #[test]
    fn limited_await_parks_without_a_question_and_registers_after_reset() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(d.path().join("limited"), "").unwrap();
        let targets = vec!["sam/r#7".to_owned()];
        // This assertion fails with the former record_wait: it returned an
        // error and saved neither intent nor an end declaration (#614).
        crate::cmd::record_wait(&cfg, &job, &[("sam/r".into(), "7".into())], &targets, None)
            .unwrap();
        assert!(job.path(PENDING).exists());
        // Even an engine that asks to retry the quota cannot publish that ask.
        crate::ask::record(
            &job,
            &crate::ask::Recommendation {
                default: "Retry after quota reset?".into(),
                one_way: false,
            },
        )
        .unwrap();
        fs::write(job.path("end.next"), "ask\n").unwrap();
        assert!(finish(&job, "1").unwrap());
        assert!(crate::ask::load(&job).is_none());
        assert!(!job.path("ask.next").exists());
        assert!(!job.turn("1", "cut").exists());
        let parked = read(&job.path("await")).unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(read(&job.path("await")).unwrap(), parked);
        assert_eq!(job.turns().as_deref(), Some("1"));
        let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
        assert!(wake[""].as_i64().unwrap() > now());
        fs::remove_file(d.path().join("limited")).unwrap();
        // Move the scheduler deadline, without sleeping or an engine turn.
        fs::write(job.path("await"), format!("{LIMIT}\t1000\t{}\n", now() - 1)).unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert!(!job.path(PENDING).exists());
        assert!(read(&job.path("await")).unwrap().starts_with("sam/r#7\t"));
        assert_eq!(read(&job.turn("1", "end")).as_deref(), Some("await"));
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert!(crate::ask::load(&job).is_none());
        fs::write(d.path().join("changed"), "").unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(read(&job.path("prompt.2")).unwrap().contains("comments"));
    }
    #[test]
    fn permission_denial_remains_a_failure() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(d.path().join("denied"), "").unwrap();
        assert!(
            crate::cmd::record_wait(
                &cfg,
                &job,
                &[("sam/r".into(), "7".into())],
                &["sam/r#7".into()],
                None
            )
            .is_err()
        );
        assert!(!job.path(PENDING).exists());
        assert!(!finish(&job, "1").unwrap());
    }
    #[test]
    fn meters_keep_installations_separate_and_hold_secondary_limits() {
        let (d, cfg) = fixture();
        let mut m = Meter::new("workers", "Sam", now() as u64);
        m.budget(&json!({"resources":{"core":{"remaining":0,"reset":now()+1000}}}));
        fs::create_dir_all(cfg.jobs.join("one")).unwrap();
        fs::create_dir_all(cfg.jobs.join("two")).unwrap();
        m.record(&cfg.jobs.join("one/github")).unwrap();
        let mut other = Meter::new("other", "sam", now() as u64);
        other.budget(&json!({"resources":{"core":{"remaining":5000,"reset":now()+2000}}}));
        other.record(&cfg.jobs.join("two/github")).unwrap();
        let git = d.path().join("git");
        burrow_test::executable(&git, "#!/bin/sh\necho https://github.com/Sam/r.git\n");
        let mut cfg = cfg;
        cfg.tools.git = git;
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("project"), d.path().to_str().unwrap()).unwrap();
        assert!(held(&cfg, &job, crate::engine::Engine::Codex).is_some());
        fs::write(job.path("app"), "other").unwrap();
        assert!(held(&cfg, &job, crate::engine::Engine::Codex).is_none());
        assert_eq!(meters(&cfg).len(), 2);
    }
}
