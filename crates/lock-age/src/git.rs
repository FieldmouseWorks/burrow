//! The checkout's git: what the refresh changed, what the base had, and the
//! remote's default branch.

use std::path::Path;
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))
}

/// The base's copy of a file, if it had one.
pub fn base_text(dir: &Path, path: &str) -> Option<String> {
    let out = git(dir, &["show", &format!("HEAD:{path}")]).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every file the refresh changed, added or left untracked, sorted.
/// NUL-separated, so no path is quoted or split.
pub fn changed(dir: &Path) -> Result<Vec<String>, String> {
    let mut paths = std::collections::BTreeSet::new();
    for args in [
        &["diff", "-z", "--name-only", "HEAD"][..],
        &["ls-files", "-z", "--others", "--exclude-standard"],
    ] {
        let out = git(dir, args)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        paths.extend(
            String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|x| !x.is_empty())
                .map(String::from),
        );
    }
    Ok(paths.into_iter().collect())
}

/// The repo's default branch, asked of the remote now: a clone Renovate
/// kept from an earlier run (persistRepoData, or one whose cleanup failed)
/// can hold an origin/HEAD from before the default changed.
pub fn default_branch(dir: &Path) -> Result<String, String> {
    let out = git(dir, &["ls-remote", "--symref", "origin", "HEAD"])?;
    let said = String::from_utf8_lossy(&out.stdout);
    let branch = out
        .status
        .success()
        .then(|| {
            said.strip_prefix("ref: refs/heads/")?
                .split_once("\tHEAD\n")
        })
        .flatten()
        .map(|(b, _)| b)
        .filter(|b| !b.is_empty() && !b.contains(char::is_whitespace));
    match branch {
        Some(b) => Ok(b.to_owned()),
        None => {
            let err = String::from_utf8_lossy(&out.stderr);
            let why = if err.trim().is_empty() {
                said.trim()
            } else {
                err.trim()
            };
            Err(format!("can't tell the default branch: {why}"))
        }
    }
}
