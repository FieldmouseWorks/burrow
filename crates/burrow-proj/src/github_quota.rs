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
    pub turn: String,
    #[serde(default)]
    pub baselines: String,
}

pub fn meters(cfg: &Config) -> Vec<Meter> {
    let mut latest: BTreeMap<(String, String), Meter> = BTreeMap::new();
    for file in cfg
        .github_meters
        .as_ref()
        .into_iter()
        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten())
        .filter_map(Result::ok)
    {
        if file.path().extension().is_none_or(|e| e != "json") {
            continue;
        }
        if let Some(m) = Meter::read(&file.path()) {
            latest.insert((m.app.clone(), m.owner.clone()), m);
        }
    }
    latest.into_values().collect()
}

pub(crate) fn held(cfg: &Config, job: &Job, engine: crate::engine::Engine) -> Option<i64> {
    admission_reset(cfg, job, engine).filter(|t| *t > now())
}
pub(crate) fn admission_reset(
    cfg: &Config,
    job: &Job,
    engine: crate::engine::Engine,
) -> Option<i64> {
    let request = crate::job::Request::load(job).ok()?;
    let app = crate::turn::github_app(cfg, request.app, engine)
        .ok()
        .flatten()?;
    let repo = read(&job.path("link"))
        .and_then(|link| crate::github::target(&link).map(|(r, _)| r))
        .unwrap_or_else(|| crate::pick::github_repo(cfg, &request.project));
    let owner = repo.split('/').next()?;
    meters(cfg)
        .into_iter()
        .filter(|m| m.app == app && m.owner.eq_ignore_ascii_case(owner))
        .filter_map(|m| m.deadline())
        .max()
        .map(|t| t as i64)
}

pub(crate) fn note(cfg: &Config, job: &Job, owner: &str, due: u64) -> Result<(), String> {
    // Host-owned direct calls are trusted, but their writes still use a
    // separate host root. Native jobs cannot publish global evidence here.
    if let (Some(app), Some(dir)) = (&cfg.host_github_app, &cfg.github_meters) {
        let mut meter = Meter::new(app, owner, now().max(0) as u64);
        meter.retry_at = Some(due);
        meter.record_public(dir).map_err(|e| e.to_string())?;
        crate::job::kick(cfg, ["proj-watch".into()]);
    }
    let _ = job;
    Ok(())
}

/// Only actual failed API calls write refusal, never quoted tool results.
pub(crate) fn refused(job: &Job) -> Option<i64> {
    let n = job.turns()?;
    refused_for(job, &n)
}
fn refused_for(job: &Job, n: &str) -> Option<i64> {
    fs::read_dir(job.turn(n, "github-refusals"))
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
        .filter_map(|e| Meter::read(&e.path()).and_then(|m| m.retry_at))
        .max()
        .map(|t| t as i64)
}

pub(crate) fn finish(job: &Job, n: &str) -> Result<bool, String> {
    if job.turn(n, "stopped").exists() {
        return Ok(false);
    }
    if read(&job.path("await")).is_some_and(|text| registered(&text)) && !owner_question(job)? {
        let _ = crate::state_reads::remove(job.path(PENDING));
        return Ok(false);
    }
    let Some(due) = state(&job.path(PENDING))
        .map_err(|e| format!("{:?}", e.say))?
        .and_then(|s| serde_json::from_str::<Pending>(&s).ok())
        .filter(|p| p.turn == n)
        .map(|p| p.retry_at as i64)
        .into_iter()
        .chain(refused(job))
        .max()
    else {
        return Ok(false);
    };
    finish_at(job, n, due)?;
    Ok(true)
}
fn finish_at(job: &Job, n: &str, due: i64) -> Result<(), String> {
    if owner_question(job)? {
        // Finalize a native or collected VM declaration before replacing any
        // answer with quota advice. Existing authority (including an expired
        // question's owner hold) keeps its identity, text and deadline.
        let expired = state(&crate::ask::host_path(job, "question-needs-owner"))
            .map_err(|e| format!("{:?}", e.say))?
            .is_some();
        if !expired && crate::ask::pending(job)?.is_some() {
            if crate::ask::load(job).is_none() {
                let text = crate::engine::answer(&job.turn(n, "jsonl"));
                if !text.is_empty() {
                    swap(job.turn(n, "result"), format!("{text}\n")).map_err(|e| e.to_string())?;
                }
            }
            // Normal ask finalization applies upgrades and retains the
            // committed question's identity, text, deadline and one-way flag.
            crate::ask::finish(job.cfg, job, "ask")?;
        }
        // The question is now the active declaration. Saved GitHub intent
        // remains durable, but its publication must not start a reset turn.
        for name in ["await", "await.next"] {
            match crate::state_reads::remove(job.path(name)) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        swap(job.turn(n, "end"), if expired { "done\n" } else { "ask\n" })
            .map_err(|e| e.to_string())?;
        swap(job.turn(n, "outcome"), "done\n").map_err(|e| e.to_string())?;
        return Ok(());
    }
    park(job, due)?;
    swap(job.turn(n, "result"), format!("GitHub quota is exhausted. This job is parked until {}; saved waits will be registered automatically.\n", crate::usage::when(due))).map_err(|e| e.to_string())?;
    swap(job.turn(n, "end"), "await\n").map_err(|e| e.to_string())?;
    swap(job.turn(n, "outcome"), "GitHub quota; waiting for reset\n").map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn park(job: &Job, due: i64) -> Result<(), String> {
    if owner_question(job)? {
        return Ok(());
    }
    let n = job
        .turns()
        .ok_or_else(|| "quota wait has no turn".to_string())?;
    swap(job.turn(&n, LIMIT), format!("{}\t{due}", now())).map_err(|e| e.to_string())?;
    swap(job.path("await"), format!("{LIMIT}\t{}\t{due}\n", now())).map_err(|e| e.to_string())?;
    crate::ask::clear(job);
    crate::ask::clear_declaration(job)?;
    Ok(())
}

/// Quota recovery may discard an uncommitted reversible retry suggestion,
/// but must not retire a committed question or a pending one-way decision.
fn owner_question(job: &Job) -> Result<bool, String> {
    let text = |path: &std::path::Path| state(path).map_err(|e| format!("{:?}", e.say));
    if let Some(question) = text(&crate::ask::host_path(job, "question"))? {
        serde_json::from_str::<crate::ask::Question>(&question).map_err(|e| e.to_string())?;
        return Ok(true);
    }
    if text(&crate::ask::host_path(job, "question-needs-owner"))?.is_some() {
        return Ok(true);
    }
    Ok(crate::ask::pending(job)?.is_some_and(|r| r.one_way))
}

pub(crate) fn registered(text: &str) -> bool {
    let rows = crate::job::awaits(text);
    !rows.is_empty()
        && rows.iter().all(|a| {
            crate::github::target(&a.target).is_some()
                && a.since.parse::<i64>().is_ok()
                && (a.was.is_empty()
                    || serde_json::from_str::<serde_json::Value>(&a.was)
                        .is_ok_and(|v| v.is_object()))
        })
}
/// The same committed deadline feeds scheduling, wake booking and watchdog
/// facts, even when publication of the await marker was interrupted.
pub(crate) fn reset(job: &Job, n: &str) -> Option<(i64, i64)> {
    // A reset cannot answer an owner question. Keep the durable quota
    // evidence for diagnostics; the question's own deadline drives its wake.
    if owner_question(job).unwrap_or(true) {
        return None;
    }
    if job.turn(n, "stopped").exists() {
        return None;
    }
    let text = match state(&job.path("await")) {
        Ok(t) => t.unwrap_or_default(),
        Err(_) => return Some((now(), now())),
    };
    if registered(&text) {
        return None;
    }
    let hold = match state(&job.turn(n, LIMIT)) {
        Ok(t) => t,
        Err(_) => return Some((now(), now())),
    };
    let hold = hold.and_then(|s| {
        s.split_once('\t')
            .and_then(|(a, b)| Some((a.parse::<i64>().ok()?, b.parse::<i64>().ok()?)))
    });
    let pending = match state(&job.path(PENDING)) {
        Ok(t) => t,
        Err(_) => return Some((now(), now())),
    };
    let parsed = pending
        .as_ref()
        .and_then(|s| serde_json::from_str::<Pending>(s).ok());
    let due = parsed
        .as_ref()
        .filter(|p| p.turn == n)
        .map(|p| p.retry_at as i64)
        .into_iter()
        .chain(hold.map(|h| h.1))
        .chain(refused_for(job, n))
        .max()
        .or_else(|| (pending.is_some() && parsed.is_none()).then_some(now()))?;
    let since = hold.map_or_else(now, |h| h.0);
    Some((since, due))
}

/// Durable intent is authoritative even when finalization never published
/// its await marker. Called by the host watcher under the job lock.
pub(crate) fn reconcile(job: &Job) -> Result<Option<i64>, crate::Fail> {
    let Some(n) = job.turns() else {
        return Ok(None);
    };
    if job.turn(&n, "stopped").exists() || matches!(job.state().as_str(), "queued" | "running") {
        return Ok(None);
    }
    if owner_question(job).map_err(|e| crate::Fail::new(1, e))? {
        // Recover a crash between collecting the declaration and committing
        // its ask, before the watcher can replay saved GitHub waits.
        if state(&job.path(PENDING))?.is_some() || refused(job).is_some() {
            finish_at(job, &n, 0).map_err(|e| crate::Fail::new(1, e))?;
        }
        // The watcher now tends this question's expiry and notification;
        // quota recovery owns no deadline for it.
        return Ok(None);
    }
    let text = state(&job.path("await"))?.unwrap_or_default();
    let awaits = crate::job::awaits(&text);
    let marker = awaits
        .iter()
        .find(|a| a.target == LIMIT)
        .and_then(|a| a.was.parse::<i64>().ok());
    let registered = registered(&text);
    if registered {
        return Ok(None);
    }
    let pending = state(&job.path(PENDING))?;
    let parsed = pending
        .as_ref()
        .and_then(|s| serde_json::from_str::<Pending>(s).ok());
    if parsed.as_ref().is_some_and(|p| p.turn != n) {
        crate::state_reads::remove(job.path(PENDING))
            .map_err(|e| crate::io_fail(&job.path(PENDING), e))?;
        return Ok(None);
    }
    let due = reset(job, &n).map(|(_, due)| due);
    if let Some(due) = due
        && (marker != Some(due) || state(&job.turn(&n, "end"))?.as_deref() != Some("await"))
    {
        finish_at(job, &n, due).map_err(|e| crate::Fail::new(1, e))?;
    }
    Ok(due)
}

#[derive(Debug)]
pub(crate) enum RecoveryError {
    Request(crate::Fail),
    State(crate::Fail),
}
impl From<crate::Fail> for RecoveryError {
    fn from(error: crate::Fail) -> Self {
        Self::State(error)
    }
}
pub(crate) fn join_records(before: &str, after: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    before
        .lines()
        .chain(after.lines())
        .filter(|line| !line.is_empty())
        .filter(|line| seen.insert(line.split('\t').next().unwrap_or_default().to_string()))
        .map(|line| format!("{line}\n"))
        .collect()
}
pub(crate) fn state(path: &std::path::Path) -> Result<Option<String>, crate::Fail> {
    match crate::state_reads::bytes(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(|text| Some(text.trim_end_matches('\n').to_owned()))
            .map_err(|e| crate::Fail::new(1, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::io_fail(path, e)),
    }
}
pub(crate) fn captured(job: &Job) -> Result<String, crate::Fail> {
    Ok(state(&job.turn(&job.turns().unwrap_or_default(), "github-awaits"))?.unwrap_or_default())
}
pub(crate) fn remember(job: &Job, waits: &str) -> Result<(), crate::Fail> {
    let mut kept = captured(job)?;
    kept = join_records(&kept, waits);
    let path = job.turn(&job.turns().unwrap_or_default(), "github-awaits");
    swap(&path, kept).map_err(|e| crate::io_fail(&path, e))
}
/// Recover an await after the reset, without asking the engine to repeat it.
/// The intent survives partial API failure and process/reboot boundaries.
pub(crate) fn replay(cfg: &Config, job: &Job) -> Result<Option<i64>, RecoveryError> {
    if owner_question(job).map_err(|e| crate::Fail::new(1, e))? {
        if let Some(n) = job.turns() {
            finish_at(job, &n, 0).map_err(|e| crate::Fail::new(1, e))?;
        }
        return Ok(None);
    }
    state(&job.path("await"))?;
    state(&job.path("await.next"))?;
    let Some(text) = state(&job.path(PENDING))? else {
        return Ok(None);
    };
    let mut pending: Pending = serde_json::from_str(&text)
        .map_err(|e| RecoveryError::Request(crate::Fail::new(1, e.to_string())))?;
    if job.turns().as_deref() != Some(pending.turn.as_str())
        || job.turn(&pending.turn, "stopped").exists()
    {
        return Ok(None);
    }
    let targets = pending.targets.clone();
    let found = targets
        .iter()
        .map(|t| {
            crate::github::target(t)
                .ok_or_else(|| crate::Fail::new(1, "invalid saved GitHub target"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(RecoveryError::Request)?;
    crate::github::take_limit();
    let captured_text = join_records(&captured(job)?, &pending.baselines);
    let captured_targets: Vec<_> = crate::job::awaits(&captured_text)
        .into_iter()
        .map(|a| a.target)
        .collect();
    let mut owner = String::new();
    let mut checkpoint_error = None;
    let result = found
        .iter()
        .zip(&targets)
        .try_fold(String::new(), |mut text, (found, target)| {
            if captured_targets.contains(&format!("{}#{}", found.0, found.1)) {
                return Ok(text);
            }
            owner = found.0.split('/').next().unwrap_or_default().to_owned();
            let baseline = crate::github::for_job(job, || {
                crate::cmd::baselines(
                    cfg,
                    std::slice::from_ref(found),
                    std::slice::from_ref(target),
                    None,
                )
            })?;
            pending.baselines = join_records(&pending.baselines, &baseline);
            if let Err(e) = swap(
                job.path(PENDING),
                serde_json::to_vec(&pending).expect("pending await"),
            )
            .map_err(|e| crate::io_fail(&job.path(PENDING), e))
            {
                checkpoint_error = Some(e);
                return Err(crate::Fail::new(1, "cannot checkpoint recovered intent"));
            }
            if let Err(e) = remember(job, &baseline) {
                checkpoint_error = Some(e);
                return Err(crate::Fail::new(1, "cannot checkpoint recovered baseline"));
            }
            text.push_str(&baseline);
            Ok(text)
        });
    if let Some(error) = checkpoint_error {
        return Err(RecoveryError::State(error));
    }
    let waits = match result {
        Ok(waits) => waits,
        Err(error) => {
            if let Some(due) = crate::github::take_limit() {
                pending.retry_at = pending.retry_at.max(due);
                swap(
                    job.path(PENDING),
                    serde_json::to_vec(&pending).expect("pending await"),
                )
                .map_err(|e| crate::io_fail(&job.path(PENDING), e))?;
                let installation_due = due;
                let due = std::iter::once(pending.retry_at)
                    .chain(refused(job).map(|t| t as u64))
                    .max()
                    .unwrap_or(due);
                park(job, due as i64).map_err(|e| crate::Fail::new(1, e))?;
                if let Err(e) = note(cfg, job, &owner, installation_due) {
                    eprintln!("proj: quota meter unavailable: {e}");
                }
                return Ok(Some(due as i64));
            }
            return Err(RecoveryError::Request(error));
        }
    };
    // Publish the complete wait before removing intent. Repeating after a
    // crash is safe: these are reads, never GitHub mutations.
    let mut kept = captured_text;
    kept = join_records(&kept, &waits);
    publish(job, &kept)?;
    fs::remove_file(job.path(PENDING)).map_err(|e| crate::io_fail(&job.path(PENDING), e))?;
    Ok(None)
}

pub(crate) fn restore(job: &Job) -> Result<(), crate::Fail> {
    if owner_question(job).map_err(|e| crate::Fail::new(1, e))? {
        if let Some(n) = job.turns() {
            finish_at(job, &n, 0).map_err(|e| crate::Fail::new(1, e))?;
        }
        return Ok(());
    }
    publish(job, &captured(job)?)
}
fn publish(job: &Job, waits: &str) -> Result<(), crate::Fail> {
    state(&job.path("await"))?;
    state(&job.path("await.next"))?;
    captured(job)?;
    let mut seen = std::collections::BTreeSet::new();
    let waits: String = waits
        .lines()
        .filter(|line| seen.insert(line.split('\t').next().unwrap_or_default()))
        .map(|line| format!("{line}\n"))
        .collect();
    let archive = job.turn(&job.turns().unwrap_or_default(), "github-awaits");
    swap(&archive, &waits).map_err(|e| crate::io_fail(&archive, e))?;
    // Checkpoint complete baselines before publication. The await file is
    // the commit point: failures before it leave the quota wait recoverable.
    swap(job.path("await.next"), &waits).map_err(|e| crate::io_fail(&job.path("await.next"), e))?;
    if let Some(n) = job.turns() {
        swap(job.turn(&n, "end"), "await\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "end"), e))?;
        swap(job.turn(&n, "outcome"), "done\n")
            .map_err(|e| crate::io_fail(&job.turn(&n, "outcome"), e))?;
    }
    swap(job.path("await"), &waits).map_err(|e| crate::io_fail(&job.path("await"), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub(super) fn fixture() -> (tempfile::TempDir, Config) {
        let d = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = d.path().join("jobs");
        cfg.github_meters = Some(d.path().join("host-meters"));
        fs::create_dir_all(cfg.github_meters.as_ref().unwrap()).unwrap();
        cfg.wake = d.path().join("wake");
        cfg.inbox = None;
        cfg.waits = None;
        // Notification tests install their own notifier. Other quota tests
        // must not invoke the example configuration's placeholder command.
        cfg.notify = None;
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
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Automated,
        )
        .unwrap();
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
        let mut intent: Pending = serde_json::from_str(&read(&job.path(PENDING)).unwrap()).unwrap();
        intent.retry_at = (now() - 1) as u64;
        swap(job.path(PENDING), serde_json::to_vec(&intent).unwrap()).unwrap();
        park(&job, now() - 1).unwrap();
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
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Automated,
        )
        .unwrap();
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
        m.record(cfg.github_meters.as_ref().unwrap()).unwrap();
        let mut other = Meter::new("other", "sam", now() as u64);
        other.budget(&json!({"resources":{"core":{"remaining":5000,"reset":now()+2000}}}));
        other.record(cfg.github_meters.as_ref().unwrap()).unwrap();
        let git = d.path().join("git");
        burrow_test::executable(&git, "#!/bin/sh\necho https://github.com/Sam/r.git\n");
        let mut cfg = cfg;
        cfg.tools.git = git;
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        let mut request = crate::job::Request {
            project: d.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        request.save(&job).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Automated,
        )
        .unwrap();
        assert!(held(&cfg, &job, crate::engine::Engine::Codex).is_some());
        // A file the job writes names no App; its request does.
        fs::write(job.path("app"), "other").unwrap();
        assert!(held(&cfg, &job, crate::engine::Engine::Codex).is_some());
        request.app = Some("other".into());
        request.save(&job).unwrap();
        assert!(held(&cfg, &job, crate::engine::Engine::Codex).is_none());
        assert_eq!(meters(&cfg).len(), 2);
    }
}

#[cfg(test)]
mod durability_tests;
#[cfg(test)]
mod owner_tests;
#[cfg(test)]
mod recovery_tests;
