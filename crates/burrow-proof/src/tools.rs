//! A trusted origin branch beside the PR head. The module's configuration
//! selects the branch; no PR file or base branch chooses the tools.
//! Its worktree lives under the same run id, so ExecStopPost and the next
//! run's sweep remove it after every process of the run has ended.
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Record, trees};

pub fn path(rec: &Record, id: &str, i: usize) -> PathBuf {
    rec.trees.join(format!("{id}-tools{i}"))
}
fn reference(id: &str) -> String {
    format!("refs/burrow-proof/{id}/tools")
}

/// Fetch to a run-specific ref: concurrent fetches cannot change which
/// commit is checked out between fetch and worktree add.
pub fn prepare(rec: &Record, id: &str, i: usize, log: &File) -> Result<(), String> {
    let Some(source) = &rec.tools_ref else {
        return Ok(());
    };
    let target = reference(id);
    let status = Command::new(&rec.git)
        .arg("-C")
        .arg(&rec.checkout)
        .args([
            "fetch",
            "-q",
            "--no-write-fetch-head",
            "origin",
            &format!("{source}:{target}"),
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log.try_clone().map_err(|e| e.to_string())?)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("can't fetch trusted tools from {source}"));
    }
    let tree = path(rec, id, i);
    let made = trees::add(&rec.git, &rec.checkout, &tree, &target, log).map_err(|e| e.to_string());
    // The worktree pins the commit. End also forgets this ref if a stop
    // interrupted fetch/add, and sweep handles a machine restart.
    forget(rec, id)?;
    if !made? {
        return Err(format!(
            "can't make trusted tools checkout at {}",
            tree.display()
        ));
    }
    let head = Command::new(&rec.git)
        .arg("-C")
        .arg(&tree)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    if !head.status.success() {
        return Err("can't identify trusted tools commit".into());
    }
    let _ = writeln!(
        &mut &*log,
        "autoland: tools {source} at {}",
        String::from_utf8_lossy(&head.stdout).trim()
    );
    Ok(())
}

pub fn forget(rec: &Record, id: &str) -> Result<(), String> {
    let status = Command::new(&rec.git)
        .arg("-C")
        .arg(&rec.checkout)
        .args(["update-ref", "-d", &reference(id)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("can't remove temporary trusted tools ref".into())
    }
}

/// Refs left by a machine restart, including a fetch stopped before its
/// worktree existed. A ref whose run is live is never removed.
pub fn sweep(rec: &Record, builds: &Path, own: &str) -> Result<(), String> {
    let output = Command::new(&rec.git)
        .arg("-C")
        .arg(&rec.checkout)
        .args(["for-each-ref", "--format=%(refname)", "refs/burrow-proof/"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("can't list temporary trusted tools refs".into());
    }
    for name in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(id) = name
            .strip_prefix("refs/burrow-proof/")
            .and_then(|s| s.strip_suffix("/tools"))
        else {
            continue;
        };
        if id != own
            && !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !trees::live(&builds.join(id))
        {
            forget(rec, id)?;
        }
    }
    Ok(())
}
