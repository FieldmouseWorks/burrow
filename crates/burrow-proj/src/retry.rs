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

use crate::{config::Config, engine, job::Job, now_nanos, read, swap, systemd, turn};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub waited: i64,
    pub said: Option<String>,
    pub deadline: Option<i64>,
    name: String,
    first: bool,
}

impl Checkpoint {
    pub fn load(job: &Job, n: &str) -> io::Result<Option<Self>> {
        match fs::read(job.turn(n, "retry-migration")) {
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                let parts: Vec<_> = state.name.splitn(3, '-').collect();
                if parts.len() != 3
                    || !matches!(parts[0], "0" | "1" | "2")
                    || parts[1].is_empty()
                    || !parts[1].bytes().all(|b| b.is_ascii_digit())
                    || parts[2] != job.id
                    || state.name.contains('/')
                {
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

    pub fn retry(job: &Job, n: &str, first: bool, waited: i64, said: &str) -> io::Result<()> {
        let state = Self {
            waited,
            said: Some(said.into()),
            first,
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
        swap(
            &job.turn(n, "retry-migration"),
            &serde_json::to_string(&state)?,
        )
    }

    pub fn resume(&self, job: &Job, n: &str, first: &Path) -> io::Result<PathBuf> {
        if let Some(dl) = self.deadline {
            swap(
                &job.turn(n, "deadline"),
                &format!("{}\n", dl + now_nanos() - self.waited),
            )?;
        }
        let prompt = if let Some(said) = &self.said {
            let asked = read(first).unwrap_or_default();
            let prompt = job.turn(n, "spent");
            swap(
                &prompt,
                &format!(
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
            fs::remove_file(job.turn(n, "quiet"))?;
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(job.turn(n, "jsonl"))?
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))?;
        fs::remove_file(job.turn(n, "retry-migration"))?;
        Ok(prompt)
    }

    pub fn queue(&self) -> (&str, i64) {
        (&self.name, self.waited)
    }
}

/// Enumerate only this engine's turn parents, using argv and executable
/// identity rather than inherited environment or a possibly recycled pid.
fn parents() -> io::Result<Vec<(u32, String, bool)>> {
    let current = std::env::current_exe()?;
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")?.filter_map(Result::ok) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if exe == current || exe.file_name().and_then(|n| n.to_str()) != Some("burrow-proj") {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<_> = bytes.split(|b| *b == 0).collect();
        let Some(i) = args.iter().position(|a| *a == b"_exec") else {
            continue;
        };
        if let Some(id) = args.get(i + 1).and_then(|b| std::str::from_utf8(b).ok()) {
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
        !e.file_name().to_string_lossy().starts_with('.')
            && read(&e.path()).as_deref() == Some(pid.to_string().as_str())
    });
    let waited = place
        .as_ref()
        .and_then(|e| crate::mtime_nanos(&e.path()))
        .or_else(|| crate::mtime_nanos(&job.turn(n, "withdrawable")))
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
    let stream = job.turn(n, "jsonl");
    let said = engine::last_result(&stream, 0)
        .and_then(|last| {
            engine::refused_file(engine::Engine::Claude, &last, &stream, 0, crate::now())
        })
        .map(|(_, said)| said);
    Ok(Checkpoint {
        waited,
        said,
        name,
        first,
        deadline: read(&job.turn(n, "deadline")).and_then(|s| s.parse().ok()),
    })
}

/// The barrier is mandatory: observing a busy legacy census alone is not a
/// barrier; the system service owns it for the entire deployment, including
/// this consumer's restarts. A health check verifies its unit is active.
pub fn pass(cfg: &Config) -> io::Result<()> {
    for (pid, id, first) in parents()? {
        let job = Job::new(cfg, &id);
        let Some(n) = job.turns() else { continue };
        let Some(_lock) = job.try_lock()? else {
            continue;
        };
        // Running turns keep their own locks and executable. Waiting Codex
        // and emergency turns must transfer too: an old initial handover can
        // itself be blocked on the fenced census. Their saved engine/flag
        // sends the replacement straight back to its independent pool.
        if job.turn(&n, "slot").exists() || job.turn(&n, "outcome").exists() {
            continue;
        }
        // Confirm this is the unit's main process, not another user's argv.
        if systemd::show(cfg, &job.unit(), "MainPID") != pid.to_string() {
            continue;
        }
        if Checkpoint::load(&job, &n)?.is_none() {
            let state = checkpoint(cfg, &job, &n, pid, first)?;
            swap(
                &job.turn(&n, "retry-migration"),
                &serde_json::to_string(&state)?,
            )?;
        }
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
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let Some(n) = job.turns() else { continue };
        let Some(state) = Checkpoint::load(&job, &n)? else {
            continue;
        };
        if job.turn(&n, "stopped").exists() {
            fs::remove_file(job.turn(&n, "retry-migration"))?;
            continue;
        }
        let Some(_lock) = job.try_lock()? else {
            continue;
        };
        if systemd::active(cfg, &job.unit()) {
            continue;
        }
        systemd::reset_failed(cfg, &job.unit());
        // Wait for the stop's end hook before reusing its transient unit name.
        let mut ready = false;
        for _ in 0..120 {
            if systemd::show(cfg, &job.unit(), "LoadState") != "loaded" && !job.end_pending() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if !ready {
            return Err(io::Error::other(format!(
                "{id} is still ending during transfer"
            )));
        }
        if job.turn(&n, "outcome").exists() {
            // A transfer which failed after startup retains its checkpoint.
            // Back off before retrying a persistent I/O/configuration failure.
            std::thread::sleep(Duration::from_secs(2));
            fs::remove_file(job.turn(&n, "outcome"))?;
        }
        if !turn::launch(cfg, &job, state.first) {
            return Err(io::Error::other(format!("couldn't resume {id}")));
        }
    }
    Ok(())
}

/// Watch both new job directories and each turn's slot removal. A scan after
/// installing watches closes the creation race; overflow also causes a full
/// rescan. No polling timer is needed for post-deploy refusals.
pub fn watch(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(cfg.jobs.join(".queue"))?;
    fs::create_dir_all(cfg.jobs.join(".slots"))?;
    let fd = inotify::init(CreateFlags::CLOEXEC)?;
    let mut watched = HashSet::new();
    let mut buf = [MaybeUninit::uninit(); 16384];
    loop {
        for path in std::iter::once(cfg.jobs.clone()).chain(
            fs::read_dir(&cfg.jobs)?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir()),
        ) {
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
        pass(cfg)?;
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
