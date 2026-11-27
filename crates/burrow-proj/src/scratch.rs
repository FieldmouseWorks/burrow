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
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::job::Job;

/// Packfile count above which a repo's common git dir gets a serialized
/// geometric repack when a finished job's worktrees are pruned.
const PACK_LIMIT: usize = 32;
/// Loose-object count above which a repo with few packfiles still gets a
/// geometric repack (which packs reachable loose objects and prunes them).
const LOOSE_LIMIT: usize = 2048;
/// How old an orphaned pack lock or temp file must be before maintenance
/// treats it as left behind by a crashed repack and clears it.
const STALE_LOCK: Duration = Duration::from_secs(3600);

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
    if crate::policy::parked(job) || crate::policy::path(job, "restoring").exists() {
        return false;
    }
    // A question whose authority can't be read is still a question.
    if !matches!(crate::ask::load(job), Ok(None)) {
        return false;
    }
    let Some(n) = job.turns().filter(|n| !n.is_empty()) else {
        return false;
    };
    if crate::record::stopped(job, &n) {
        return true;
    }
    if crate::record::awaiting(job) {
        return false;
    }
    crate::record::end(job, &n).as_deref() == Some("done")
        || crate::job::Request::load(job).is_ok_and(|r| r.owned)
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
        maintain(&cfg.tools.git, &common);
    }
}

/// Serialized, bounded repo maintenance on job end: under a non-blocking
/// per-repo lock, clear stale pack locks/temps left by crashed repacks and
/// collapse excess packfiles with an incremental geometric repack.
fn maintain(git: &Path, common: &Path) {
    let lock_path = common.join("burrow-gc.lock");
    let Ok(lock) = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    else {
        return;
    };
    if lock.try_lock().is_err() {
        return;
    }
    let objects = common.join("objects");
    let pack_dir = objects.join("pack");
    let now = SystemTime::now();
    let mut packs = 0usize;
    let mut active_midx_lock = false;
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".pack")
                && !name.starts_with(".tmp-")
                && !name.starts_with("tmp_pack_")
            {
                packs += 1;
                continue;
            }
            let is_midx_lock = name == "multi-pack-index.lock";
            let is_orphan_tmp = name.starts_with("tmp_pack_")
                || name.starts_with("tmp_idx_")
                || name.starts_with("tmp_rev_")
                || name.starts_with(".tmp-");
            if !is_midx_lock && !is_orphan_tmp {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= STALE_LOCK);
            if stale {
                let _ = fs::remove_file(&path);
            } else if is_midx_lock {
                active_midx_lock = true;
            }
        }
    }
    if active_midx_lock || (packs <= PACK_LIMIT && !excess_loose(&objects, LOOSE_LIMIT)) {
        return;
    }
    let ok = Command::new(git)
        .arg("--git-dir")
        .arg(common)
        .args([
            "-c",
            "pack.threads=2",
            "-c",
            "pack.windowMemory=256m",
            "-c",
            "pack.packSizeLimit=512m",
            "repack",
            "-q",
            "-d",
            "-l",
            "--geometric=2",
            "--write-midx",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("proj: couldn't repack {}", common.display());
    }
}

/// Whether `objects`'s 2-hex-digit loose-object buckets hold more than `limit`
/// entries, stopping as soon as the limit is crossed.
fn excess_loose(objects: &Path, limit: usize) -> bool {
    let Ok(buckets) = fs::read_dir(objects) else {
        return false;
    };
    let mut count = 0usize;
    for bucket in buckets.flatten() {
        let name = bucket.file_name();
        let name = name.to_string_lossy();
        if name.len() != 2 || !name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            continue;
        }
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for _ in entries.flatten() {
            count += 1;
            if count > limit {
                return true;
            }
        }
    }
    false
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
