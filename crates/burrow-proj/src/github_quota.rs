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
    /// Targets whose baseline is still to be taken.
    pub targets: Vec<String>,
    /// Complete wait records already captured, as `proj await` wrote them.
    #[serde(default)]
    pub baselines: String,
    pub retry_at: u64,
}

/// Complete, newline-terminated wait records: all of `old`, then those of
/// `new` whose target `old` lacks. The first baseline of a target stands.
pub(crate) fn merge(old: &str, new: &str) -> String {
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for line in old.lines().chain(new.lines()).filter(|l| !l.is_empty()) {
        let target = line.split('\t').next().unwrap_or_default();
        if !seen.contains(&target) {
            seen.push(target);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// What a replay came to.
pub(crate) enum Replay {
    /// Published; the waits are in place.
    Done,
    /// Refused again: held until then.
    Held(i64),
    /// GitHub said no to a saved target. The intent stays until a recovery
    /// turn is accepted, so a failed start cannot lose the error.
    Failed(String),
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

/// The host's own client (`tools.gh`) authenticates as the machinery App, not
/// as the job's: a refusal it meets holds that installation, so the queue
/// keeps admitting jobs of Apps it did not touch. No machinery App means the
/// host's gh is the owner's own login, which has no installation to hold.
pub(crate) fn note(cfg: &Config, job: &Job, owner: &str, due: u64) -> Result<(), String> {
    if let Some(app) = cfg.machinery.as_deref().filter(|a| !a.is_empty()) {
        let mut meter = Meter::new(app, owner, now().max(0) as u64);
        meter.retry_at = Some(due);
        meter
            .record(&job.path("github"))
            .map_err(|e| e.to_string())?;
        // Every queued turn of this installation now waits for the reset.
        crate::waits::rewatch_all(cfg);
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
        .into_iter()
        .chain(refused(job))
        .max()
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
/// The intent survives partial API failure and process/reboot boundaries; an
/// `Err` is a host failure (state that could not be written), to be retried.
pub(crate) fn replay(cfg: &Config, job: &Job) -> Result<Replay, crate::Fail> {
    let path = job.path(PENDING);
    let mut p: Pending = match read(&path).map(|text| serde_json::from_str(&text)) {
        Some(Ok(p)) => p,
        Some(Err(e)) => return Ok(Replay::Failed(format!("saved GitHub await: {e}"))),
        None => Pending {
            targets: Vec::new(),
            baselines: String::new(),
            retry_at: 0,
        },
    };
    let mut found = Vec::new();
    for t in &p.targets {
        match crate::github::target(t) {
            Some(f) => found.push(f),
            None => return Ok(Replay::Failed(format!("invalid saved GitHub target {t}"))),
        }
    }
    crate::github::take_limit();
    let text = match crate::cmd::baselines_each(cfg, &found, &p.targets, None) {
        Ok(text) => text,
        Err(failed) => {
            if let Some(due) = crate::github::take_limit() {
                // The installation the host client used, and the intent
                // and wait with it, so every refusal renews the hold.
                note(cfg, job, &failed.owner, due).map_err(|e| crate::Fail::new(1, e))?;
                p.baselines = merge(&p.baselines, &failed.done);
                p.targets.drain(..failed.count);
                p.retry_at = due;
                swap(&path, serde_json::to_vec(&p).expect("pending await"))
                    .map_err(|e| crate::io_fail(&path, e))?;
                park(job, due as i64).map_err(|e| crate::Fail::new(1, e))?;
                return Ok(Replay::Held(due as i64));
            }
            return Ok(Replay::Failed(
                failed
                    .error
                    .say
                    .unwrap_or_else(|| format!("exit {}", failed.error.code)),
            ));
        }
    };
    // Publish the complete wait before removing intent. Repeating after a
    // crash is safe: these are reads, never GitHub mutations. Baselines taken
    // before the refusal are restored as they were, each its own record.
    let kept = merge(
        &merge(
            &read(&job.path("await.next")).unwrap_or_default(),
            &p.baselines,
        ),
        &text,
    );
    swap(job.path("await"), &kept).map_err(|e| crate::io_fail(&job.path("await"), e))?;
    if let Some(n) = job.turns() {
        swap(job.turn(&n, "end"), "await\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "end"), e))?;
        swap(job.turn(&n, "outcome"), "done\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "outcome"), e))?;
    }
    let _ = fs::remove_file(job.path("await.next"));
    retire(job);
    Ok(Replay::Done)
}

/// Saved intent is consumed once recovery or a new turn owns it, so it cannot
/// park a later turn again.
pub(crate) fn retire(job: &Job) {
    let _ = fs::remove_file(job.path(PENDING));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub(super) fn fixture() -> (tempfile::TempDir, Config) {
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
        let mut p: Pending = serde_json::from_str(&read(&job.path(PENDING)).unwrap()).unwrap();
        p.retry_at = now() as u64 - 1;
        fs::write(job.path(PENDING), serde_json::to_vec(&p).unwrap()).unwrap();
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
    fn an_ordinary_replay_error_keeps_the_intent_until_a_recovery_turn_starts() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(d.path().join("denied"), "").unwrap();
        for bad in [
            r#"{"targets":["sam/r#7"],"retry_at":1}"#,
            r#"{"targets":["junk"],"retry_at":1}"#,
            "not json",
        ] {
            fs::write(job.path(PENDING), bad).unwrap();
            assert!(matches!(replay(&cfg, &job).unwrap(), Replay::Failed(_)));
            assert!(
                job.path(PENDING).exists(),
                "{bad}: a failed start must not lose it"
            );
            assert!(!finish(&job, "1").unwrap() || read(&job.path("await")).is_some());
        }
    }
    #[test]
    fn merged_replay_keeps_each_record_whole_and_the_first_baseline() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        // A's record has no trailing newline once read; B is pending; C was
        // captured before the refusal and is kept with its own timestamp.
        fs::write(job.path("await.next"), "sam/a#1\t1\t{}\n").unwrap();
        fs::write(
            job.path(PENDING),
            r#"{"targets":["sam/r#7"],"baselines":"sam/c#3\t5\t{}\nsam/a#1\t9\t{\"x\":1}","retry_at":1}"#,
        )
        .unwrap();
        assert!(matches!(replay(&cfg, &job).unwrap(), Replay::Done));
        let published = read(&job.path("await")).unwrap();
        let lines: Vec<_> = published.lines().collect();
        assert_eq!(lines.len(), 3, "{published}");
        assert_eq!(lines[0], "sam/a#1\t1\t{}", "the first baseline stands");
        assert_eq!(lines[1], "sam/c#3\t5\t{}");
        assert!(lines[2].starts_with("sam/r#7\t"));
        assert!(!job.path(PENDING).exists());
        let _ = d;
    }
    #[test]
    fn a_refusal_part_way_keeps_the_records_already_read() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        // The stand-in gh refuses only a target named "limit".
        fs::write(d.path().join("limited"), "").unwrap();
        fs::write(
            job.path(PENDING),
            r#"{"targets":["sam/r#7","sam/r#8"],"baselines":"","retry_at":1}"#,
        )
        .unwrap();
        assert!(matches!(replay(&cfg, &job).unwrap(), Replay::Held(_)));
        let p: Pending = serde_json::from_str(&read(&job.path(PENDING)).unwrap()).unwrap();
        assert_eq!(
            p.targets.len(),
            2,
            "nothing was read before the first refusal"
        );
        fs::remove_file(d.path().join("limited")).unwrap();
        assert!(matches!(replay(&cfg, &job).unwrap(), Replay::Done));
        assert_eq!(read(&job.path("await")).unwrap().lines().count(), 2);
    }
    #[test]
    fn committed_intent_parks_a_job_whose_await_marker_is_missing_or_early() {
        let (_d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        job.mark_ended();
        let due = now() + 3600;
        fs::write(
            job.path(PENDING),
            format!(r#"{{"targets":["sam/r#7"],"retry_at":{due}}}"#),
        )
        .unwrap();
        for before in [None, Some(format!("{LIMIT}\t1\t{}\n", now() - 5))] {
            let _ = fs::remove_file(job.path("await"));
            if let Some(b) = &before {
                fs::write(job.path("await"), b).unwrap();
            }
            crate::watch::look(&cfg, "").unwrap();
            let marker = read(&job.path("await")).unwrap();
            assert!(
                marker.starts_with(LIMIT) && marker.trim().ends_with(&due.to_string()),
                "{marker}"
            );
            assert_eq!(job.turns().as_deref(), Some("1"), "no engine turn");
        }
        // A stopped job is left alone.
        let _ = fs::remove_file(job.path("await"));
        fs::write(job.turn("1", "stopped"), "").unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert!(!job.path("await").exists());
    }
    #[test]
    fn replay_refusal_renews_the_machinery_installation_and_keeps_peers_eligible() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(d.path().join("limited"), "").unwrap();
        fs::write(job.path(PENDING), r#"{"targets":["Sam/r#7"],"retry_at":1}"#).unwrap();
        let Replay::Held(due) = replay(&cfg, &job).unwrap() else {
            panic!("held")
        };
        assert!(job.path(PENDING).exists(), "intent stays through a refusal");
        let meters = meters(&cfg);
        assert_eq!(meters.len(), 1);
        assert_eq!(
            (meters[0].app.as_str(), meters[0].owner.as_str()),
            ("workers", "sam")
        );
        assert_eq!(meters[0].until(now() as u64), Some(due as u64));
    }
    #[test]
    fn baselines_taken_before_a_later_refusal_are_restored_without_a_turn() {
        let (d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.path("await.next"), "sam/a#1\t1\t{}\n").unwrap();
        fs::create_dir_all(job.path("github")).unwrap();
        let mut refusal = Meter::new("workers", "sam", now() as u64);
        refusal.retry_at = Some(now() as u64 + 3600);
        refusal.record(&job.path("github")).unwrap();
        fs::write(
            job.path("github/refusal"),
            serde_json::to_vec(&refusal).unwrap(),
        )
        .unwrap();
        assert!(finish(&job, "1").unwrap());
        assert!(read(&job.path("await")).unwrap().starts_with(LIMIT));
        fs::write(job.path("await"), format!("{LIMIT}\t1000\t{}\n", now() - 1)).unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(read(&job.path("await")).unwrap(), "sam/a#1\t1\t{}");
        assert_eq!(job.turns().as_deref(), Some("1"), "no engine turn");
        assert!(!job.path("await.next").exists());
        let _ = d;
    }
    #[test]
    fn the_longest_live_deadline_parks_the_job() {
        let (_d, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(job.path("github")).unwrap();
        job.set_turns(1).unwrap();
        let mut refusal = Meter::new("workers", "sam", now() as u64);
        refusal.retry_at = Some(now() as u64 + 7200);
        fs::write(
            job.path("github/refusal"),
            serde_json::to_vec(&refusal).unwrap(),
        )
        .unwrap();
        fs::write(
            job.path(PENDING),
            format!(r#"{{"targets":[],"retry_at":{}}}"#, now() + 60),
        )
        .unwrap();
        assert!(finish(&job, "1").unwrap());
        let due: i64 = read(&job.path("await"))
            .unwrap()
            .trim()
            .rsplit('\t')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(due, now() + 7200);
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

#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn usage_is_one_json_value_for_any_mix_of_hold_and_readings() {
        let (_d, cfg) = tests::fixture();
        assert!(crate::cmd::usage_value(&cfg).is_none(), "nothing to say");
        for n in 1..=3 {
            let mut m = Meter::new("workers", &format!("owner{n}"), now() as u64);
            m.retry_at = Some(now() as u64 + 3600);
            fs::create_dir_all(cfg.jobs.join("j")).unwrap();
            m.record(&cfg.jobs.join("j/github")).unwrap();
            let v = crate::cmd::usage_value(&cfg).unwrap();
            assert_eq!(v["github"].as_array().unwrap().len(), n);
            assert!(v.get("until").is_none());
        }
        // Claude's hold keeps its fields beside the array.
        crate::usage::hold(&cfg, now() + 3600, "out").unwrap();
        let v = crate::cmd::usage_value(&cfg).unwrap();
        assert!(v["until"].as_i64().unwrap() > now());
        assert_eq!(v["github"].as_array().unwrap().len(), 3);
    }
}
