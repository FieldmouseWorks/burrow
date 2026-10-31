//! Each job's scratch: SCRATCH/jobs/ID, its turns' PROJ_SCRATCH, where it makes
//! its worktrees, clones and builds. It goes, with every worktree in it, once
//! the job is over, so nothing a finished job built (cargo's target dirs run to
//! gigabytes a worktree) is left to pile up. `proj _ended` looks at the end of
//! each turn, and `proj stop` at a job it ends between turns; the job's own end
//! is the event, never a timer.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};

use crate::config::Config;
use crate::job::Job;
use crate::{filled, read};

/// Where job ID's scratch is.
pub fn dir(cfg: &Config, id: &str) -> PathBuf {
    cfg.scratch.join("jobs").join(id)
}

/// Whether the job is over as its latest turn left it, so nothing gives it
/// another turn of its own accord: `proj stop` ended it, or it awaits nothing
/// and either said `proj done` or is --owned (its starter acts on each turn's
/// end itself, and starts a new job for anything more). One that awaits
/// GitHub, asks a person, or ended any other way (a failure, no word on what
/// comes next) keeps its scratch: what it awaits, or a reply, resumes it there.
/// A reply to a job that was over finds its scratch made afresh, empty.
pub fn over(job: &Job) -> bool {
    let Some(n) = job.turns().filter(|n| !n.is_empty()) else {
        return false;
    };
    if job.turn(&n, "stopped").exists() {
        return true;
    }
    if filled(&job.path("await")) {
        return false;
    }
    read(&job.turn(&n, "end")).as_deref() == Some("done") || job.path("owned").exists()
}

/// Job ID's scratch gone, and each worktree in it taken off the repo it's a
/// worktree of (git worktree prune), so its branch can be checked out again.
/// It's renamed out of the way first, which only one of two at once can do, so
/// a turn that starts next makes its own afresh and a second clear (the turn's
/// end and `proj stop`, both) finds nothing to do. Only once no turn of the job
/// runs: whatever still worked in it would lose its files. An id that isn't
/// one name, which no job has, is left alone: it would name someone else's.
pub fn clear(cfg: &Config, id: &str) {
    if id.is_empty() || id == "." || id == ".." || id.contains('/') {
        return;
    }
    let gone = cfg
        .scratch
        .join("jobs")
        .join(format!(".gone-{id}-{}", process::id()));
    if fs::rename(dir(cfg, id), &gone).is_err() {
        return;
    }
    let repos = worktree_repos(cfg, &gone);
    // What a build left read-only (a Go module cache, a store path copied out)
    // stops the removal until it's writable again.
    if fs::remove_dir_all(&gone).is_err() {
        writable(&gone);
        if let Err(e) = fs::remove_dir_all(&gone) {
            eprintln!(
                "proj: couldn't remove all of job {id}'s scratch, left in {}: {e}",
                gone.display()
            );
        }
    }
    for common in repos {
        let pruned = Command::new(&cfg.tools.git)
            .arg("--git-dir")
            .arg(&common)
            .args(["worktree", "prune"])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !pruned {
            eprintln!(
                "proj: couldn't prune {}'s worktrees after job {id}",
                common.display()
            );
        }
    }
}

/// The repos outside `gone` that hold a worktree in it. A worktree's .git is a
/// file naming its repo, at whatever depth the job made it; a clone's own repo
/// (a .git dir, not looked inside) goes with it.
fn worktree_repos(cfg: &Config, gone: &Path) -> BTreeSet<PathBuf> {
    let mut repos = BTreeSet::new();
    let mut dirs = vec![gone.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else {
                continue;
            };
            let git = e.file_name() == ".git";
            if kind.is_dir() && !git {
                dirs.push(e.path());
            } else if kind.is_file()
                && git
                && let Some(common) = common_dir(cfg, &d)
                && !common.starts_with(gone)
            {
                repos.insert(common);
            }
        }
    }
    repos
}

/// The repo a worktree at this path is of, by git.
fn common_dir(cfg: &Config, worktree: &Path) -> Option<PathBuf> {
    let out = Command::new(&cfg.tools.git)
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let mut path = out.stdout;
    if path.last() == Some(&b'\n') {
        path.pop();
    }
    (!path.is_empty()).then(|| OsString::from_vec(path).into())
}

/// Every directory under this path open to its owner again, so what's in them
/// can go. Symlinks aren't followed.
fn writable(path: &Path) {
    let is_dir = fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    if !is_dir {
        return;
    }
    if let Ok(m) = fs::metadata(path) {
        let mut p = m.permissions();
        p.set_mode(p.mode() | 0o700);
        let _ = fs::set_permissions(path, p);
    }
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            writable(&e.path());
        }
    }
}

#[cfg(test)]
mod tests;
