//! Forgetting a repository under the inbox pass lock.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::{drain, enrollment, key};
use crate::config::Config;
use crate::job::Job;
use crate::{Fail, io_fail, read};

/// The lock, once whoever holds it lets go, if that's within `wait`.
pub(super) fn lock_within(f: &File, wait: Duration) -> io::Result<bool> {
    let until = Instant::now() + wait;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                sleep(Duration::from_millis(100));
            }
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// All the inbox kept about a repo's issues (which job each is, what of it
/// has been answered, that the repo has the label, that it's out of reach),
/// gone, so the next pass takes each open one up as new: the steward's
/// (autoland's repos.py), when GitHub's repo of that name is another one than
/// the deleted repo it was. Its issue N isn't the deleted one's N, and none of
/// the old job's answers, or its session, are its. The old jobs are the
/// steward's to stop. Run under the pass's lock, so no pass is under way.
pub(super) fn forget(cfg: &Config, state: &Path, repo: &str) -> Result<(), Fail> {
    // A legacy worker stays terminal even after every record of the old
    // repository is forgotten. Sorting records carry no worker association.
    for d in enrollment::worker_records(state) {
        let Some(issue) = read(&d.join("issue")) else {
            continue;
        };
        let Some((of, n)) = issue.rsplit_once('#') else {
            continue;
        };
        if !of.eq_ignore_ascii_case(repo) {
            continue;
        }
        if let Some(id) = read(&d.join("job")) {
            let job = Job::new(cfg, &id);
            if job.dir.is_dir() {
                let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                enrollment::remember(&job, of, n).map_err(|e| Fail::new(1, e))?;
            }
        }
    }
    // GitHub's names are case-blind: the inbox kept the clone's spelling.
    let want = key(repo).to_lowercase();
    let names = |dir: &Path| -> Vec<(String, PathBuf)> {
        fs::read_dir(dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (name, path) in names(state) {
        let name = name.to_lowercase();
        // The label's is .label-KEY; an issue's a folder, KEY-N.
        let failed = if let Some(k) = name.strip_prefix(".label-") {
            k == want && fs::remove_file(&path).is_err()
        } else if !name.starts_with('.')
            && name.rsplit_once('-').is_some_and(|(k, _)| k == want)
            && path.is_dir()
        {
            fs::remove_dir_all(&path).is_err()
        } else {
            false
        };
        if failed {
            return Err(Fail::new(
                1,
                format!("proj-inbox: couldn't remove {}", path.display()),
            ));
        }
    }
    for namespace in [".triage", ".plans"] {
        for (name, path) in names(&state.join(namespace)) {
            if name
                .to_lowercase()
                .rsplit_once('-')
                .is_some_and(|(k, _)| k == want)
            {
                fs::remove_dir_all(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    for (name, path) in names(&state.join(".triage-waiting")) {
        if name
            .to_lowercase()
            .rsplit_once('-')
            .is_some_and(|(k, _)| k == want)
        {
            fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
        }
    }
    for namespace in [".retired", ".parked"] {
        for (name, path) in names(&state.join(namespace)) {
            if name.eq_ignore_ascii_case(&want) {
                fs::remove_dir_all(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    for dir in [".unreached", drain::READY_LISTS] {
        for (name, path) in names(&state.join(dir)) {
            if !name.starts_with('.') && name.to_lowercase() == want {
                fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    Ok(())
}
