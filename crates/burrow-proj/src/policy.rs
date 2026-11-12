//! Durable repository identity and vacating. A pause is a parked job, never a
//! stop: its brief, session, scratch, question and awaits remain its own.
use crate::{config::Config, job::Job, now, read, swap, systemd};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "repo", rename_all = "camelCase")]
pub enum Identity {
    Repository(String),
    Local,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Kind {
    Manual,
    Sorting,
    Automated,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Admission {
    identity: Identity,
    kind: Kind,
    agent: Option<String>,
}

pub(crate) fn path(job: &Job, name: &str) -> std::path::PathBuf {
    crate::ask::host_path(job, &format!("policy-{name}"))
}
pub fn bind(job: &Job, identity: Identity, kind: Kind) -> Result<(), String> {
    crate::ask::ensure(job)?;
    let record = Admission {
        identity,
        kind,
        agent: read(&job.path("agent")).filter(|s| !s.is_empty()),
    };
    swap(
        &path(job, "admission"),
        &serde_json::to_string(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub fn kind(role: Option<&str>, inbox: bool) -> Kind {
    match role {
        Some("triage" | "planner") => Kind::Sorting,
        Some(_) => Kind::Automated,
        None if inbox => Kind::Automated,
        None => Kind::Manual,
    }
}

/// Whether work started from inside job `id` is automated: unless that job is
/// bound as manual (or is unknown or unreadable, which fails closed).
pub fn automated_parent(cfg: &Config, id: &str) -> bool {
    let job = Job::new(cfg, id);
    !job.dir.is_dir() || admission(&job).map_or(true, |a| !matches!(a.kind, Kind::Manual))
}

pub fn identify(
    cfg: &Config,
    project: &str,
    automatic: bool,
    local: bool,
) -> Result<Identity, String> {
    let repo = crate::pick::github_repo(cfg, project);
    if !repo.is_empty() {
        return Ok(Identity::Repository(repo));
    }
    if local || !automatic {
        Ok(Identity::Local)
    } else {
        Err("repository identity is unresolved; local automation requires --local-work".into())
    }
}

fn admission(job: &Job) -> Result<Admission, String> {
    match fs::read(path(job, "admission")) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let source = read(&crate::ask::host_path(job, "source"))
                .and_then(|s| s.split_once('#').map(|(repo, _)| repo.to_owned()));
            let identity = match source {
                Some(repo) => Identity::Repository(repo),
                None => identify(job.cfg, &crate::pick::project(&job.dir), true, false)?,
            };
            // Legacy automation is never inferred to be local from a failed
            // lookup. Modern launchers bind identity before exposing job state.
            bind(job, identity, Kind::Automated)?;
            admission(job)
        }
        Err(e) => Err(e.to_string()),
    }
}
pub fn denied(job: &Job) -> bool {
    let Ok(a) = admission(job) else {
        return true;
    };
    let Identity::Repository(repo) = a.identity else {
        return false;
    };
    let Some(settings) = &job.cfg.settings else {
        return true;
    };
    let Ok(d) = settings.decision(&repo) else {
        return true;
    };
    d.vacate || matches!(a.kind, Kind::Automated) && !d.allowed
}

/// Bound identity for downstream runtime readers, never mutable job metadata.
pub(crate) fn repository(job: &Job) -> Option<String> {
    match admission(job).ok()?.identity {
        Identity::Repository(repo) => Some(repo),
        Identity::Local => None,
    }
}

#[derive(Serialize, Deserialize)]
pub struct Park {
    pub state: String,
    pub turn: String,
    pub first: bool,
    pub at: i64,
}

pub fn parked(job: &Job) -> bool {
    path(job, "park").exists()
}

pub fn mark(job: &Job, state: &str, mut first: bool) -> Result<(), String> {
    let turn = job.turns().unwrap_or_default();
    if let Ok(bytes) = fs::read(path(job, "park")) {
        let prior: Park = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if prior.turn == turn {
            return Ok(());
        }
        first |= prior.first;
    }
    let retry = crate::retry::Checkpoint::load(job, &turn).ok().flatten();
    if let Some(checkpoint) = &retry {
        first |= checkpoint.first;
    }
    let record = Park {
        state: state.into(),
        turn: turn.clone(),
        first,
        at: now(),
    };
    let mut order = None;
    if let Ok(entries) = fs::read_dir(job.cfg.jobs.join(".queue")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if crate::slot::job_of(&name) == job.id && !name.starts_with('.') {
                order = Some(name);
            }
        }
    }
    let order = order
        .or_else(|| read(&path(job, "order")))
        .or_else(|| read(&job.turn(&turn, "queue-order")))
        .or_else(|| retry.map(|checkpoint| checkpoint.queue().0.to_owned()))
        .unwrap_or_else(|| {
            format!(
                "{}-{}-{}",
                if first { 1 } else { 2 },
                crate::mtime_nanos(&job.path(&format!("prompt.{turn}")))
                    .unwrap_or_else(crate::now_nanos),
                job.id
            )
        });
    swap(&path(job, "order"), &order).map_err(|e| e.to_string())?;
    swap(
        &path(job, "park"),
        &serde_json::to_string(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    wake(job.cfg);
    Ok(())
}

/// Runs in its own independent unit, so vacating the caller cannot kill the
/// reconciler. All records are committed before stopping any process group.
pub fn reconcile(cfg: &Config) -> Result<(), String> {
    fs::create_dir_all(&cfg.jobs).map_err(|e| e.to_string())?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cfg.jobs.join(".policy.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    loop {
        let before = generation(cfg)?;
        reconcile_pass(cfg, &before)?;
        if before == generation(cfg)? {
            return Ok(());
        }
    }
}

fn overlay(cfg: &Config) -> Result<Vec<u8>, String> {
    let Some(settings) = &cfg.settings else {
        return Ok(Vec::new());
    };
    match fs::read(&settings.overlay) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

type Generation = (Vec<u8>, Option<i64>, Option<String>);

fn generation(cfg: &Config) -> Result<Generation, String> {
    Ok((
        overlay(cfg)?,
        // A malformed policy is still denied by each admission reader;
        // fingerprint its bytes without preventing vacating.
        cfg.settings
            .as_ref()
            .and_then(|s| s.scope_until().ok().flatten()),
        read(&cfg.jobs.join(".policy-wake")),
    ))
}

fn reconcile_pass(cfg: &Config, release_generation: &Generation) -> Result<(), String> {
    let mut ids: Vec<_> = fs::read_dir(&cfg.jobs)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort_by_key(|id| read(&path(&Job::new(cfg, id), "order")).unwrap_or_else(|| id.clone()));
    let mut errors = Vec::new();
    for id in ids {
        if let Err(error) = reconcile_job(cfg, &id, release_generation) {
            errors.push(format!("{id}: {error}"));
        }
    }
    if let Err(error) = crate::policy_proofs::reconcile(cfg) {
        errors.push(error);
    }
    // What the watchdog watches, its waits as the policy leaves them: a job
    // it denied or parked drops out, and one it released rewatched itself.
    // Never every job there is, which would put back long-finished ones
    // whose waits were settled weeks ago.
    crate::waits::rewatch_all(cfg);
    if let Some(until) = cfg
        .settings
        .as_ref()
        .and_then(|s| s.scope_until().ok().flatten())
        && read(&cfg.jobs.join(".policy-expiry")).as_deref() != Some(&until.to_string())
    {
        let ok = systemd::systemd_run(cfg)
            .args(["--quiet", "--collect"])
            .arg(format!("--unit=proj-policy-expiry-{until}"))
            .arg(format!("--on-active={}s", (until - now()).max(1)))
            .arg("--timer-property=AccuracySec=1s")
            .arg(&cfg.me)
            .arg("_policy")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err("could not schedule policy expiry".into());
        }
        swap(&cfg.jobs.join(".policy-expiry"), &until.to_string()).map_err(|e| e.to_string())?;
    }
    use std::hash::{Hash, Hasher};
    let mut generation = std::collections::hash_map::DefaultHasher::new();
    if let Some(settings) = &cfg.settings {
        serde_json::to_string(settings)
            .map_err(|e| e.to_string())?
            .hash(&mut generation);
        match fs::read(&settings.overlay) {
            Ok(bytes) => bytes.hash(&mut generation),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        settings.scope_until()?.hash(&mut generation);
    }
    let generation = generation.finish().to_string();
    if read(&cfg.jobs.join(".policy-generation")).as_deref() != Some(&generation) {
        crate::job::kick(
            cfg,
            ["proj-watch".into(), "proj-inbox".into(), "autoland".into()],
        );
        swap(&cfg.jobs.join(".policy-generation"), &generation).map_err(|e| e.to_string())?;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn reconcile_job(cfg: &Config, id: &str, release_generation: &Generation) -> Result<(), String> {
    let job = Job::new(cfg, id);
    if path(&job, "restoring").exists() && !parked(&job) && systemd::gone(cfg, &job.unit()) {
        let rec: Park =
            serde_json::from_slice(&fs::read(path(&job, "restoring")).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if job.turns().as_deref() != Some(&rec.turn) {
            // No _exec accepted this prepared restoration; retain its
            // prompt as an unstarted attempt, as ordinary launch does.
            job.set_turns(rec.turn.parse().map_err(|_| "invalid parked turn")?)
                .map_err(|e| e.to_string())?;
        }
        fs::rename(path(&job, "restoring"), path(&job, "park")).map_err(|e| e.to_string())?;
    }
    if job.turns().is_none() {
        return Ok(());
    }
    if denied(&job) {
        // Activating and deactivating units can still hold descendants and
        // inherited locks; only a known gone unit has vacated.
        let active = !systemd::gone(cfg, &job.unit());
        if !parked(&job) {
            let state = job.state();
            if !active && job.holds()?.is_empty() {
                return Ok(());
            }
            let first = job
                .turns()
                .is_some_and(|n| read(&job.turn(&n, "priority")).as_deref() == Some("first"));
            mark(&job, &state, first)?;
        }
        // stop kills the complete cgroup: inherited job, heavy and engine
        // locks are freed even when descendants ignored TERM.
        if active {
            kill(cfg, &job.unit())?;
        }
        if let Ok(a) = admission(&job)
            && let Some(agent) = a.agent
        {
            agent_command(cfg, "stop", &agent)?;
        }
        remove_places(cfg, id)?;
    } else if parked(&job) {
        // Turning off is immediate; turning on must traverse one ordered
        // generation. Otherwise an unpause mid-pass can release a later
        // job after an earlier one was already skipped under the old pause.
        if &generation(cfg)? != release_generation {
            return Ok(());
        }
        // Both can block; a policy change meanwhile ends this pass, and the
        // next one restarts from the front of the order.
        let held = job.lock().map_err(|e| e.to_string())?;
        if &generation(cfg)? != release_generation {
            return Ok(());
        }
        crate::inbox::enrollment::admit(&job)?;
        let crate::inbox::resumption::Resume::Ready(_capacity) =
            crate::inbox::resumption::admission(cfg, &job)?
        else {
            return Ok(());
        };
        if &generation(cfg)? != release_generation {
            return Ok(());
        }
        let mut rec: Park =
            serde_json::from_slice(&fs::read(path(&job, "park")).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if !systemd::gone(cfg, &job.unit()) {
            return Ok(());
        }
        shift_waits(&job, now().saturating_sub(rec.at))?;
        rec.at = now();
        swap(
            &path(&job, "park"),
            &serde_json::to_string(&rec).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        // A release starts a fresh stall clock but retains queue order.
        swap(&path(&job, "released"), &now().to_string()).map_err(|e| e.to_string())?;
        fs::rename(path(&job, "park"), path(&job, "restoring")).map_err(|e| e.to_string())?;
        if let Ok(a) = admission(&job)
            && let Some(agent) = a.agent
        {
            agent_command(cfg, "start", &agent)?;
        }
        let result = if rec.state == "queued" {
            if job.retry_pending(&rec.turn) {
                crate::retry::Identity::restore(&job, &rec.turn)?;
            }
            crate::turn::launch(cfg, &job, rec.first)
                .then_some(())
                .ok_or_else(|| format!("could not restore queued {id}"))
        } else if rec.state == "running" {
            let note = "The owner paused this repository and stopped the whole turn. Its worktrees and history were kept. Resume where you left off; inspect completed work before repeating any command.";
            let prompt = if crate::filled(&job.path("session")) {
                note.to_owned()
            } else {
                format!(
                    "{}\n\n{note}",
                    read(&job.path(&format!("prompt.{}", rec.turn))).unwrap_or_default()
                )
            };
            crate::turn::start_admitted(cfg, id, &prompt, rec.first, &_capacity)
        } else {
            Ok(())
        };
        if let Err(e) = result {
            let _ = fs::rename(path(&job, "restoring"), path(&job, "park"));
            return Err(e);
        }
        if !matches!(rec.state.as_str(), "running" | "queued") {
            let _ = fs::remove_file(path(&job, "restoring"));
        }
        drop(held);
        crate::waits::rewatch(cfg, &[id]);
        crate::job::kick(
            cfg,
            ["proj-watch".into(), "proj-inbox".into(), "autoland".into()],
        );
    }
    Ok(())
}

fn remove_places(cfg: &Config, id: &str) -> Result<(), String> {
    if let Ok(entries) = fs::read_dir(cfg.jobs.join(".queue")) {
        for e in entries.flatten() {
            if crate::slot::job_of(&e.file_name().to_string_lossy()) == id {
                match fs::remove_file(e.path()) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
    }
    let _ = fs::remove_file(cfg.jobs.join(".slots/.next"));
    Ok(())
}

pub(crate) fn kill(cfg: &Config, unit: &str) -> Result<(), String> {
    let _ = systemd::systemctl(cfg)
        .args(["kill", "--kill-whom=all", "--signal=SIGKILL", unit])
        .status();
    if systemd::stop(cfg, unit) != 0 && !systemd::gone(cfg, unit) {
        return Err(format!("could not vacate {unit}"));
    }
    Ok(())
}

fn shift_waits(job: &Job, elapsed: i64) -> Result<(), String> {
    if let Some(mut question) = crate::ask::load(job) {
        question.due = question.due.saturating_add(elapsed);
        swap(
            &crate::ask::host_path(job, "question"),
            &serde_json::to_string(&question).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(text) = read(&job.path("await")) {
        let shifted: String = crate::job::awaits(&text)
            .into_iter()
            .map(|mut a| {
                if let Ok(at) = a.since.parse::<i64>() {
                    a.since = at.saturating_add(elapsed).to_string();
                }
                a.line()
            })
            .collect();
        swap(&job.path("await"), &shifted).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Restoration is admitted only once its place is visible to the scheduler.
pub(crate) fn accepted(job: &Job) -> Result<(), String> {
    match fs::remove_file(path(job, "restoring")) {
        Ok(()) => {
            wake(job.cfg);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// systemd accepting a unit does not mean its queue entry is published yet.
/// Reserve earlier restored places until their owning pool has registered them.
pub(crate) fn earlier_pending(job: &Job) -> bool {
    let Some(order) = read(&path(job, "order")) else {
        return false;
    };
    let Ok(choice) = crate::engine::choice(job.cfg, job) else {
        return true;
    };
    fs::read_dir(&job.cfg.jobs)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            if id == job.id || id.starts_with('.') {
                return false;
            }
            let other = Job::new(job.cfg, &id);
            path(&other, "restoring").exists()
                && read(&path(&other, "order")).is_some_and(|earlier| earlier < order)
                && !denied(&other)
                && crate::engine::choice(job.cfg, &other).is_ok_and(|c| c.engine == choice.engine)
        })
}

fn agent_command(cfg: &Config, verb: &str, id: &str) -> Result<(), String> {
    let tool = cfg
        .agent_tool
        .as_ref()
        .ok_or("agent control is not configured")?;
    if !std::process::Command::new(tool)
        .args([verb, id])
        .status()
        .is_ok_and(|s| s.success())
    {
        return Err(format!("could not {verb} agent {id}"));
    }
    Ok(())
}

/// An explicit stop supersedes a park; release must never resurrect it.
/// The caller holds the job's turn-start lock and has stopped its unit.
pub(crate) fn cancel(job: &Job) -> Result<(), String> {
    for name in ["park", "restoring", "order", "released"] {
        match fs::remove_file(path(job, name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    remove_places(job.cfg, job.id)
}

/// Job completions and newly parked turns wake control directly, even with
/// the inbox/event dispatcher stopped. The epoch retains wakes during a pass.
pub(crate) fn wake(cfg: &Config) {
    let Some(unit) = &cfg.policy_unit else {
        return;
    };
    if let Err(error) = swap(
        &cfg.jobs.join(".policy-wake"),
        &crate::now_nanos().to_string(),
    ) {
        eprintln!("proj: could not retain policy wake: {error}");
    }
    let _ = systemd::systemctl(cfg)
        .args(["start", "--no-block", unit])
        .output();
}

#[cfg(test)]
mod parent_tests {
    use super::*;

    fn bound(cfg: &Config, id: &str, kind: Option<Kind>) {
        let job = Job::new(cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        if let Some(kind) = kind {
            bind(&job, Identity::Local, kind).unwrap();
        }
    }

    #[test]
    fn only_a_manual_parent_starts_manual_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        bound(&cfg, "manual", Some(Kind::Manual));
        bound(&cfg, "sorting", Some(Kind::Sorting));
        bound(&cfg, "auto", Some(Kind::Automated));
        assert!(!automated_parent(&cfg, "manual"));
        assert!(automated_parent(&cfg, "sorting"));
        assert!(automated_parent(&cfg, "auto"));
        // Unknown parents fail closed.
        assert!(automated_parent(&cfg, "gone"));
    }
}
