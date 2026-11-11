//! Transfer waiting turns across a deploy without starting another turn.
//!
//! The deployment's barrier owns the original census lock. Older schedulers
//! therefore cannot admit a retained retry while this event consumer moves
//! its existing turn to the current executable. Running turns, Codex and the
//! emergency pool retain their own locks and continue normally.
use std::collections::HashSet;
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use serde::{Deserialize, Serialize};

mod authority;
pub(crate) use authority::Identity;

use crate::{config::Config, engine, job::Job, now_nanos, read, swap, systemd, turn};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub waited: i64,
    pub said: Option<String>,
    pub deadline: Option<i64>,
    name: String,
    first: bool,
    /// An initial waiter differs from a turn which already ran, including an
    /// unlimited retry whose refusal format the reader cannot recognize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained: Option<bool>,
}

impl Checkpoint {
    pub fn load(job: &Job, n: &str) -> io::Result<Option<Self>> {
        match crate::state_reads::bytes(&job.turn(n, "retry-migration")) {
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if !Self::valid_place(&state.name, job.id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid retry queue place",
                    ));
                }
                Ok(Some(state))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn valid_place(name: &str, id: &str) -> bool {
        let parts: Vec<_> = name.splitn(3, '-').collect();
        parts.len() == 3
            && matches!(parts[0], "0" | "1" | "2")
            && !parts[1].is_empty()
            && parts[1].bytes().all(|b| b.is_ascii_digit())
            && parts[2] == id
            && !name.contains('/')
    }

    pub fn initial(&self) -> bool {
        !self
            .retained
            .unwrap_or(self.deadline.is_some() || self.said.is_some())
    }

    pub fn retry(job: &Job, n: &str, first: bool, waited: i64, said: &str) -> io::Result<()> {
        let state = Self {
            waited,
            said: Some(said.into()),
            first,
            retained: Some(true),
            deadline: read(&job.turn(n, "deadline")).and_then(|s| s.parse().ok()),
            name: crate::slot::entry_at(
                job,
                job.id,
                if first {
                    crate::pick::Class::First
                } else {
                    crate::pick::Class::Normal
                },
                waited,
            ),
        };
        loop {
            let held = job.lock()?;
            if job.turns().as_deref() != Some(n) || job.turn(n, "stopped").exists() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "retry was cancelled or superseded",
                ));
            }
            Identity::capture(job, n).map_err(io::Error::other)?;
            match census(job, n) {
                Ok(_census) => {
                    return swap(
                        &job.turn(n, "retry-migration"),
                        &serde_json::to_string(&state)?,
                    );
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Cancellation and emergency promotion remain available.
                    drop(held);
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn resume(&self, job: &Job, n: &str, first: &Path) -> io::Result<PathBuf> {
        let _job = job.lock()?;
        if job.turns().as_deref() != Some(n)
            || job.turn(n, "stopped").exists()
            || !job.turn(n, "retry-migration").exists()
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "retry was cancelled or superseded",
            ));
        }
        // A missing or redirected prompt is not an empty conversation. Leave
        // the checkpoint pending until its actual prompt can be opened safely.
        let asked = String::from_utf8_lossy(&crate::state_reads::bytes(first)?).into_owned();
        if !self.initial()
            && let Some(dl) = self.deadline
        {
            crate::state_reads::write(
                job.turn(n, "deadline"),
                format!("{}\n", dl + now_nanos() - self.waited),
            )?;
        }
        let prompt = if let Some(said) = &self.said {
            let prompt = job.turn(n, "spent");
            crate::state_reads::write(
                &prompt,
                format!(
                    "{}\n\n{said}. {}\n\n{asked}",
                    turn::SPENT,
                    crate::usage::policy(job.cfg)
                ),
            )?;
            prompt
        } else {
            first.to_path_buf()
        };
        // Match an in-process retry: waiting consumed neither its turn
        // budget nor its next run's idle allowance. Keep the stream contents.
        if job.turn(n, "quiet").exists() {
            crate::state_reads::remove(job.turn(n, "quiet"))?;
        }
        crate::state_reads::append(&job.turn(n, "jsonl"))?
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))?;
        crate::state_reads::remove(job.turn(n, "retry-migration"))?;
        let _ = crate::state_reads::remove(job.turn(n, "retry-startup-failed"));
        Ok(prompt)
    }

    pub fn queue(&self) -> (&str, i64) {
        (&self.name, self.waited)
    }
}

fn is_codex(job: &Job, n: &str) -> io::Result<bool> {
    Ok(Identity::load(job, n)
        .map_err(io::Error::other)?
        .map_or_else(
            || read(&job.turn(n, "engine")).as_deref() == Some("codex"),
            |identity| identity.choice.engine == engine::Engine::Codex,
        ))
}

fn census(job: &Job, n: &str) -> io::Result<Option<fs::File>> {
    if job.path("unstick").exists() || is_codex(job, n)? {
        return Ok(None);
    }
    let file = fs::File::create(job.cfg.jobs.join(".slots/.admission-lock"))?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(Some(file))
}

/// Pending ordinary turns remain in the census even with no live parent.
/// The caller owns the admission lock when publishing their saved places.
pub(crate) fn admission(cfg: &Config) -> io::Result<Vec<(String, String, i64)>> {
    let mut pending = Vec::new();
    for entry in fs::read_dir(&cfg.jobs)?.filter_map(Result::ok) {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let Some(n) = job.turns() else { continue };
        if !job.retry_pending(&n) || job.path("unstick").exists() || is_codex(&job, &n)? {
            continue;
        }
        if let Some(state) = Checkpoint::load(&job, &n)? {
            pending.push((id, state.name, state.waited));
        }
    }
    Ok(pending)
}

/// Enumerate only this engine's turn parents, using argv and executable
/// identity rather than inherited environment or a possibly recycled pid.
fn parents(cfg: &Config) -> io::Result<Vec<(u32, String, bool)>> {
    let current = std::env::current_exe()?;
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")?.filter_map(Result::ok) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if exe == current {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<_> = bytes.split(|b| *b == 0).collect();
        if exe.file_name().and_then(|n| n.to_str()) != Some("burrow-proj")
            && !authority::legacy_handover(cfg, &exe, &args)
        {
            continue;
        }
        let Some(i) = args.iter().position(|a| *a == b"_exec") else {
            continue;
        };
        if let Some(id) = args.get(i + 1).and_then(|b| std::str::from_utf8(b).ok()) {
            // argv is not authority to address host paths outside this queue.
            if id.starts_with('.') || id.contains('/') {
                continue;
            }
            found.push((
                pid,
                id.to_owned(),
                args.get(i + 2).is_some_and(|s| *s == b"first"),
            ));
        }
    }
    Ok(found)
}

fn checkpoint(cfg: &Config, job: &Job, n: &str, pid: u32, first: bool) -> io::Result<Checkpoint> {
    let queue = cfg.jobs.join(".queue");
    let place = fs::read_dir(&queue)?.filter_map(Result::ok).find(|e| {
        Checkpoint::valid_place(&e.file_name().to_string_lossy(), job.id)
            && read(&e.path()).as_deref() == Some(pid.to_string().as_str())
    });
    let stream = job.turn(n, "jsonl");
    let said = engine::last_result(&stream, 0)
        .and_then(|last| {
            engine::refused_file(engine::Engine::Claude, &last, &stream, 0, crate::now())
        })
        .map(|(_, said)| said);
    // A legacy parent can be preempted after refusal but before it publishes
    // its place. Publication after reset must not turn that retained refusal
    // into fresh work that can bypass the draining cohort.
    let refusal = said.as_ref().and_then(|_| crate::mtime_nanos(&stream));
    let awaiting = read(&job.path("await"))
        .filter(|s| s.starts_with("usage-limit\t"))
        .and_then(|_| crate::mtime_nanos(&job.path("await")));
    let waited = [
        place.as_ref().and_then(|e| crate::mtime_nanos(&e.path())),
        crate::mtime_nanos(&job.turn(n, "withdrawable")),
        refusal,
        awaiting,
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or_else(now_nanos);
    let name = place
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            crate::slot::entry_at(
                job,
                job.id,
                if first {
                    crate::pick::Class::First
                } else {
                    crate::pick::Class::Normal
                },
                waited,
            )
        });
    Ok(Checkpoint {
        waited,
        said,
        name,
        first,
        retained: Some(fs::metadata(&stream).is_ok_and(|m| m.len() > 0)),
        deadline: read(&job.turn(n, "deadline")).and_then(|s| s.parse().ok()),
    })
}

/// The barrier is mandatory: observing a busy legacy census alone is not a
/// barrier; the system service owns it for the entire deployment, including
/// this consumer's restarts. A health check verifies its unit is active.
pub fn pass(cfg: &Config) -> io::Result<()> {
    // Repair the intermediate generation's serialized admission field before
    // replacing parents: still-running old writers must be able to record a
    // refusal while the rest of the transfer is in progress. A busy usage
    // lock defers this repair without blocking progress in independent pools.
    let release_error = crate::usage::admission_cutoff(cfg).err();
    let mut busy = false;
    for (pid, id, first) in parents(cfg)? {
        let job = Job::new(cfg, &id);
        if !job.dir.is_dir() || systemd::show(cfg, &job.unit(), "MainPID") != pid.to_string() {
            continue;
        }
        let Some(_lock) = job.try_lock()? else {
            busy = true;
            continue;
        };
        let Some(n) = job.turns() else { continue };
        // Running turns keep their own locks and executable. Waiting Codex
        // and emergency turns must transfer too: an old initial handover can
        // itself be blocked on the fenced census. Their saved engine/flag
        // sends the replacement straight back to its independent pool.
        if job.turn(&n, "slot").exists()
            || job.turn(&n, "outcome").exists()
            || job.turn(&n, "stopped").exists()
        {
            continue;
        }
        // Confirm this is the unit's main process, not another user's argv.
        if systemd::show(cfg, &job.unit(), "MainPID") != pid.to_string() {
            continue;
        }
        if authority::holds_slot(cfg, pid)? {
            continue;
        }
        if Checkpoint::load(&job, &n)?.is_none() && !authority::legacy_waiting(cfg, pid)? {
            busy = true;
            continue;
        }
        crate::ask::ensure(&job).map_err(io::Error::other)?;
        if Checkpoint::load(&job, &n)?.is_none() {
            let state = checkpoint(cfg, &job, &n, pid, first)?;
            Identity::capture_legacy(&job, &n).map_err(io::Error::other)?;
            let _census = match census(&job, &n) {
                Ok(held) => held,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    busy = true;
                    continue;
                }
                Err(e) => return Err(e),
            };
            swap(
                &job.turn(&n, "retry-migration"),
                &serde_json::to_string(&state)?,
            )?;
        }
        // Validate the committed checkpoint before authorizing a stop.
        Checkpoint::load(&job, &n)?
            .ok_or_else(|| io::Error::other("missing transfer checkpoint"))?;
        // The checkpoint commits before stopping anything. The end hook and
        // watcher leave it alone; a crash or failed start is retried below.
        if systemd::stop(cfg, &job.unit()) != 0 {
            return Err(io::Error::other(format!(
                "couldn't stop {} for transfer",
                job.unit()
            )));
        }
    }
    for entry in fs::read_dir(&cfg.jobs)?.filter_map(Result::ok) {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let Some(_lock) = job.try_lock()? else {
            busy = true;
            continue;
        };
        // One locked snapshot owns cancellation, turn identity and launch.
        let Some(n) = job.turns() else { continue };
        if job.turn(&n, "stopped").exists() || job.retry_completed(&n) {
            match crate::state_reads::remove(job.turn(&n, "retry-migration")) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e),
            }
            continue;
        }
        let Some(state) = Checkpoint::load(&job, &n)? else {
            continue;
        };
        if systemd::active(cfg, &job.unit()) {
            continue;
        }
        systemd::reset_failed(cfg, &job.unit());
        // A unit still ending is deferred, without holding up other pools.
        if systemd::show(cfg, &job.unit(), "LoadState") == "loaded" || job.end_pending() {
            busy = true;
            continue;
        }
        let attempt = job.turn(&n, "retry-attempt");
        if read(&attempt)
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|at| now_nanos() - at < 2_000_000_000)
        {
            busy = true;
            continue;
        }
        // Publish the rate limit before every launch, including startups that
        // cannot write an outcome. A crash cannot bypass the next delay.
        let prepare = || -> io::Result<()> {
            swap(&attempt, &now_nanos().to_string())?;
            Identity::restore(&job, &n).map_err(io::Error::other)?;
            if job.turn(&n, "outcome").exists() {
                crate::state_reads::remove(job.turn(&n, "outcome"))?;
            }
            Ok(())
        };
        if let Err(error) = prepare() {
            eprintln!("proj: couldn't prepare retained retry {id}: {error}");
            busy = true;
            continue;
        }
        if !turn::launch(cfg, &job, state.first) {
            busy = true;
            continue;
        }
    }
    if let Some(error) = release_error {
        return Err(error);
    }
    if busy {
        // The consumer retries this deferred pass explicitly. No lock file
        // events (including our own opens/closes) can create a scan loop.
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "retry transfer job lock busy",
        ));
    }
    Ok(())
}

/// Watch both new job directories and each turn's slot removal. A scan after
/// installing watches closes the creation race; overflow also causes a full
/// rescan. No polling timer is needed for post-deploy refusals.
pub fn watch(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(cfg.jobs.join(".queue"))?;
    fs::create_dir_all(cfg.jobs.join(".slots"))?;
    fs::create_dir_all(crate::ask::control_root(cfg))?;
    let fd = inotify::init(CreateFlags::CLOEXEC)?;
    let mut watched = HashSet::new();
    let mut buf = [MaybeUninit::uninit(); 16384];
    loop {
        for path in [cfg.jobs.clone(), crate::ask::control_root(cfg)]
            .into_iter()
            .chain(
                fs::read_dir(&cfg.jobs)?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir()),
            )
            .chain(
                fs::read_dir(crate::ask::control_root(cfg))?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir()),
            )
        {
            if watched.insert(path.clone()) {
                inotify::add_watch(
                    &fd,
                    &path,
                    WatchFlags::CREATE
                        | WatchFlags::MOVED_TO
                        | WatchFlags::DELETE
                        | WatchFlags::CLOSE_WRITE,
                )?;
            }
        }
        match pass(cfg) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Contention is deferred work, not a failed deployment unit.
                // Retry explicitly after releasing every per-job lock.
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            Err(e) => return Err(e),
        }
        let mut events = inotify::Reader::new(&fd, &mut buf);
        loop {
            let event = events.next()?;
            let relevant = event.file_name().is_none_or(|name| {
                let name = name.to_string_lossy();
                name.ends_with(".slot")
                    || name.ends_with(".withdrawable")
                    || name.ends_with(".retry-migration")
                    || name.ends_with(".retry-wake")
                    || (name.starts_with(|c: char| c.is_ascii_digit()) && name.contains('-'))
                    || cfg.jobs.join(name.as_ref()).is_dir()
            });
            if relevant {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests;
