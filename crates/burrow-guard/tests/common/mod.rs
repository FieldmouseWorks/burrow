//! The programs the guard's tests stand in for zfs and git, written once per
//! test binary.

#![allow(dead_code)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
