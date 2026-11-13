//! Work a job hasn't published, kept before its scratch goes. A worktree with
//! changes to tracked files or files git doesn't ignore (a build's output is
//! ignored, and goes with the scratch as before) is copied to
//! JOB/preserved/NAME.N: its branch and head, `tracked.patch` (everything
//! tracked that differs from head, staged or not) and `untracked/`. When it
//! can't be kept, the scratch is not removed: it stays for a reply or a retry.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::job::Job;

/// Every working tree in a scratch: a directory with a `.git` in it, a file
/// (worktree) or a directory (clone), at any depth.
fn trees(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_name() == ".git" {
                found.push(d.clone());
            } else if e.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(e.path());
            }
        }
    }
    found.sort();
    found
}

fn git(cfg: &Config, tree: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(&cfg.tools.git)
        .arg("-C")
        .arg(tree)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {args:?} in {}: {}",
            tree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Keep what every working tree in job `id`'s scratch has not published.
pub(super) fn unfinished(cfg: &Config, id: &str) -> Result<(), String> {
    let root = super::dir(cfg, id);
    let job = Job::new(cfg, id);
    for tree in trees(&root) {
        let dirty = git(
            cfg,
            &tree,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        if dirty.is_empty() {
            continue;
        }
        let rel = tree.strip_prefix(&root).unwrap_or(&tree);
        let name = rel.to_string_lossy().replace('/', "-");
        let base = job.path("preserved");
        fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        let mut n = 1;
        let dest = loop {
            let d = base.join(format!("{name}.{n}"));
            if !d.exists() {
                break d;
            }
            n += 1;
        };
        let part = base.join(format!(".{name}.{n}.partial"));
        let _ = fs::remove_dir_all(&part);
        keep(cfg, &tree, &part).inspect_err(|_| {
            let _ = fs::remove_dir_all(&part);
        })?;
        fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        eprintln!(
            "proj: kept {}'s unfinished work in {} before clearing its scratch",
            tree.display(),
            dest.display()
        );
    }
    Ok(())
}

fn keep(cfg: &Config, tree: &Path, to: &Path) -> Result<(), String> {
    let put = |name: &str, data: &[u8]| {
        fs::write(to.join(name), data).map_err(|e| format!("{}: {e}", to.join(name).display()))
    };
    let untracked = to.join("untracked");
    fs::create_dir_all(&untracked).map_err(|e| format!("{}: {e}", untracked.display()))?;
    put("from", tree.as_os_str().as_encoded_bytes())?;
    put(
        "branch",
        &git(cfg, tree, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(),
    )?;
    put(
        "head",
        &git(cfg, tree, &["rev-parse", "HEAD"]).unwrap_or_default(),
    )?;
    // An unborn branch has no head to differ from: only untracked files then.
    put(
        "tracked.patch",
        &git(cfg, tree, &["diff", "HEAD", "--binary"]).unwrap_or_default(),
    )?;
    let listed = git(
        cfg,
        tree,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    for rel in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let rel = Path::new(std::str::from_utf8(rel).map_err(|e| e.to_string())?);
        let from = tree.join(rel);
        let into = untracked.join(rel);
        if let Some(parent) = into.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let meta = fs::symlink_metadata(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            symlink(target, &into).map_err(|e| format!("{}: {e}", into.display()))?;
        } else if meta.is_file() {
            fs::copy(&from, &into).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    Ok(())
}
