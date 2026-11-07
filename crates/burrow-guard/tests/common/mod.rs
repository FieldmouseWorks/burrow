//! The programs the guard's tests stand in for zfs and git, written once per
//! test binary.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The stand-ins' directory, written the first time it's asked for and
/// never again: a file being written while another test starts a process
/// is open for writing in that process until it execs, and running the
/// file then fails with "Text file busy". So every helper that starts a
/// process, or has the guard start one, asks for this first: none starts
/// one while these are written, and a test's own zfs is a link to them.
pub fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        // Logs what it's asked beside where it's linked from, and fails,
        // saying what `fail` there holds, when that file is there.
        script(
            &bin.path().join("zfs"),
            "#!/bin/sh\nd=${0%/*}\necho \"$@\" >> \"$d/zfs.log\"\n[ ! -e \"$d/fail\" ] || { cat \"$d/fail\" >&2; exit 1; }\n",
        );
        // A user's service manager: logs what it's asked to run beside where
        // it's linked from, and runs it.
        script(
            &bin.path().join("systemd-run"),
            "#!/bin/sh\nd=${0%/*}\necho \"$@\" >> \"$d/manager.log\"\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nexec \"$@\"\n",
        );
        // A git that never answers.
        script(&bin.path().join("hang"), "#!/bin/sh\nexec sleep 30\n");
        bin
    })
    .path()
}

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A systemd-run in `dir` that logs what it's asked to `dir/manager.log` and
/// runs it.
pub fn systemd_run(dir: &Path) -> PathBuf {
    let path = dir.join("systemd-run");
    symlink(stand_ins().join("systemd-run"), &path).unwrap();
    path
}

/// A zfs in `dir` that logs what it's asked to `dir/zfs.log`, and fails,
/// saying what `dir/fail` holds, when that file is there.
pub fn zfs(dir: &Path) -> PathBuf {
    let zfs = dir.join("zfs");
    symlink(stand_ins().join("zfs"), &zfs).unwrap();
    zfs
}
