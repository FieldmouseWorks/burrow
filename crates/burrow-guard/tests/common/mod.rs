//! The programs the guard's tests stand in for zfs and git, written once per
//! test binary, and the repositories and hook input they're run with.

#![allow(dead_code)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use burrow_guard::{Hook, Input};
use tempfile::TempDir;

/// Shared stand-ins, installed by burrow_test and cached for the test binary.
pub fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        // Logs what it's asked beside where it's linked from, and fails,
        // saying what `fail` there holds, when that file is there.
        script(
            bin.path().join("zfs"),
            "#!/bin/sh\nd=${0%/*}\necho \"$@\" >> \"$d/zfs.log\"\n[ ! -e \"$d/fail\" ] || { cat \"$d/fail\" >&2; exit 1; }\n",
        );
        // A git that never answers.
        script(bin.path().join("hang"), "#!/bin/sh\nexec sleep 30\n");
        bin
    })
    .path()
}

use burrow_test::executable as script;

/// A zfs in `dir` that logs what it's asked to `dir/zfs.log`, and fails,
/// saying what `dir/fail` holds, when that file is there.
pub fn zfs(dir: &Path) -> PathBuf {
    let zfs = dir.join("zfs");
    symlink(stand_ins().join("zfs"), &zfs).unwrap();
    zfs
}

/// The hook's input for a Bash `cmd` run in `cwd`.
pub fn bash(cmd: &str, cwd: &Path) -> Hook {
    Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=Invented",
            "-c",
            "user.email=invented@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A clone on `branch`, tracking origin's `branch`.
pub fn clone_on(branch: &str) -> TempDir {
    clone_tracking(branch, branch)
}

/// A clone on `branch`, tracking origin's `upstream`.
pub fn clone_tracking(branch: &str, upstream: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    git(d, &["init", "-q", "-b", branch]);
    git(d, &["commit", "-q", "--allow-empty", "-m", "invented"]);
    git(d, &["remote", "add", "origin", "/dev/null"]);
    git(
        d,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{upstream}"),
            "HEAD",
        ],
    );
    git(d, &["config", &format!("branch.{branch}.remote"), "origin"]);
    git(
        d,
        &[
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{upstream}"),
        ],
    );
    dir
}
