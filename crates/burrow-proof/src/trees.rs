//! A run's worktrees: one per check, detached at the PR's head, under the
//! record's `trees` (`ID-I`), made just before its command and removed
//! once its fence is empty. `end` removes whatever its run left; a run
//! killed with no `end` at all (the machine going down) leaves its
//! worktrees, which the next run of any repo sweeps up once nothing of
//! that run can still be in them.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{CGROUP, END, Record};

/// TREE, a worktree of CHECKOUT detached at HEAD; what git says goes to LOG.
pub fn add(git: &Path, checkout: &Path, tree: &Path, head: &str, log: &File) -> io::Result<bool> {
    let status = Command::new(git)
        .arg("-C")
        .arg(checkout)
        .args(["worktree", "add", "-q", "--detach"])
        .arg(tree)
        .arg(head)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .status()?;
    Ok(status.success())
}

/// TREE and everything in it gone, a directory its command made read-only
/// included; then CHECKOUT forgets every worktree of its that's gone.
pub fn remove(git: &Path, checkout: &Path, tree: &Path) -> io::Result<()> {
    match fs::symlink_metadata(tree) {
        Ok(_) => {
            writable(tree)?;
            fs::remove_dir_all(tree)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    prune(git, checkout)
}

/// CHECKOUT's record of its worktrees, without those that are gone.
pub fn prune(git: &Path, checkout: &Path) -> io::Result<()> {
    let out = Command::new(git)
        .arg("-C")
        .arg(checkout)
        .args(["worktree", "prune"])
        .stdin(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "git -C {} worktree prune: {}",
            checkout.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Every directory under DIR (itself included, links not followed) its
/// owner may change, so all of it can be removed.
fn writable(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Ok(());
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in fs::read_dir(dir)? {
        writable(&entry?.path())?;
    }
    Ok(())
}

/// The worktrees under TREES of runs that are over, each run's folder in
/// BUILDS (ID's own aside): over once it has its `end.json`, or once the
/// cgroup its `cgroup` names is gone, so that no process of it is left;
/// a run that never wrote one never made a worktree. Each is removed,
/// and its checkout forgets it. What couldn't be, in words, for the log.
pub fn sweep(trees: &Path, builds: &Path, own: &str, git: &Path) -> Vec<String> {
    let mut said = Vec::new();
    let entries = match fs::read_dir(trees) {
        Ok(e) => e,
        Err(e) => return vec![format!("can't read {}: {e}", trees.display())],
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((id, _)) = name.rsplit_once('-') else {
            continue;
        };
        let run = builds.join(id);
        if id == own || live(&run) {
            continue;
        }
        let checkout = Record::read(&run).ok().map(|r| r.checkout);
        let gone = match &checkout {
            Some(c) => remove(git, c, &entry.path()),
            None => writable(&entry.path()).and_then(|()| fs::remove_dir_all(entry.path())),
        };
        if let Err(e) = gone {
            said.push(format!("can't sweep {}: {e}", entry.path().display()));
        }
    }
    said
}

/// Whether a process of the run whose folder is RUN may still be at work.
fn live(run: &Path) -> bool {
    !run.join(END).exists()
        && fs::read_to_string(run.join(CGROUP)).is_ok_and(|c| Path::new(c.trim_end()).exists())
}
