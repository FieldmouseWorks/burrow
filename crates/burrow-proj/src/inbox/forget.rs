//! Forgetting a repository under the inbox pass lock.

use crate::inbox::record::files;
use std::fs;
use std::path::{Path, PathBuf};

use super::{enrollment, key};
use crate::config::Config;
use crate::job::Job;
use crate::record::JobFile;
use crate::{Fail, io_fail};

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
        let Some(issue) = files::issue(&d) else {
            continue;
        };
        let Some((of, n)) = issue.rsplit_once('#') else {
            continue;
        };
        if !of.eq_ignore_ascii_case(repo) {
            continue;
        }
        if let Some(id) = files::job(&d) {
            let job = Job::new(cfg, &id);
            if job.dir.is_dir() {
                let _held = job
                    .lock()
                    .map_err(|e| io_fail(&job.file(JobFile::Lock), e))?;
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
    for root in [files::sorting_root(state), state.join(".plans")] {
        for (name, path) in names(&root) {
            if name
                .to_lowercase()
                .rsplit_once('-')
                .is_some_and(|(k, _)| k == want)
            {
                fs::remove_dir_all(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    for (name, path) in names(&files::sorting_waits(state)) {
        if name
            .to_lowercase()
            .rsplit_once('-')
            .is_some_and(|(k, _)| k == want)
        {
            fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
        }
    }
    for root in [files::retired_root(state), files::parked_root(state)] {
        for (name, path) in names(&root) {
            if name.eq_ignore_ascii_case(&want) {
                fs::remove_dir_all(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    for root in [
        files::unreached(state),
        files::ready_lists(state),
        state.join(super::scope::LISTED),
    ] {
        for (name, path) in names(&root) {
            if !name.starts_with('.') && name.to_lowercase() == want {
                fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
    }
    Ok(())
}
