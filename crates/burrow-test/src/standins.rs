//! The programs a burrow binary runs as the machine's, written once.
//!
//! A harness asks [`stand_ins`] for the ones it needs and adds only stand-ins
//! for programs of its own (`grub-editenv`, `fwupdmgr`, `nvme`...). Every one
//! keeps how it was run in `$FAKE/NAME`, `FAKE` being a dir of the machine's
//! own (or `fake/` beside the dir the program was linked into, when the
//! harness runs a library in-process and has no environment to set), and
//! answers by what is in `$FAKE`:
//!
//! - `sudo` runs what follows `--` with none of the environment it was
//!   started with, as real sudo does, but `FAKE`, `PATH`, `IN_SUDO` (it never
//!   runs inside itself, which would never end), `XDG_RUNTIME_DIR` (root's)
//!   and every variable named `FAKE_...` that a harness's own stand-ins need.
//! - `runuser -u USER -- PROGRAM ARGS` runs PROGRAM as it is. It keeps
//!   `XDG_RUNTIME_DIR` as it saw it in `$FAKE/runuser.xdg`.
//! - `systemctl`, `journalctl` and `proj` log their words and say nothing,
//!   succeeding. `proj` logs each word, then the end of the run (see [`proj_jobs`]) and says `job-N` for the Nth run; it fails,
//!   with the code `$FAKE/fail-proj` holds (1 when it's empty), when that is
//!   there.
//! - `systemd-cat` keeps its words and then what it is given on stdin in
//!   `$FAKE/journal`; it fails when `$FAKE/fail-systemd-cat` is there.
//! - `box-notify` keeps its words, joined by `|`, in `$FAKE/notify`; it fails
//!   when `$FAKE/fail-notify` is there.
//!
//! What a machine needs one to answer is a shell script in `$FAKE/NAME.says`,
//! sourced after the log (a machine's own data, like `$FAKE/updates`, not a
//! stand-in): its words are the stand-in's, and what it leaves is the exit,
//! except for `sudo` and `runuser`, which run their program after it unless it
//! exits.
//!
//! Every one finds `cat`, `env` and `sed` on the PATH the test was started
//! with, whatever PATH the program under test gives it, and passes the PATH it
//! was given on (`sudo`, `runuser`); `systemd-cat` finds `cat` on the PATH it
//! was given, as the real one finds the program it runs.

use std::fs;
use std::path::Path;
use std::process::Output;

/// The programs [`stand_ins`] writes. A harness never defines one of its own:
/// the `owned` test fails when one does.
pub const OWNED: [&str; 7] = [
    "sudo",
    "systemctl",
    "journalctl",
    "systemd-cat",
    "runuser",
    "box-notify",
    "proj",
];

/// Everything but the name's own words: the log, the answer, the exit.
const HEAD: &str =
    "#!/bin/sh\ngiven=$PATH PATH='@PATH@'\n: \"${FAKE:=${0%/*}/../fake}\"\nexport FAKE\n";

const SAYS: &str = "[ ! -e \"$FAKE/@NAME@.says\" ] || . \"$FAKE/@NAME@.says\"\n";

fn script(name: &str) -> String {
    let says = SAYS.replace("@NAME@", name);
    let body = match name {
        "sudo" => format!(
            "[ -z \"$IN_SUDO\" ] || exit 9\n\
             echo \"$*\" >>\"$FAKE/sudo\"\n\
             [ \"$1\" = -- ] || exit 9\n\
             shift\n\
             {says}\
             for v in $(env | sed -n 's/^\\(FAKE_[A-Za-z0-9_]*\\)=.*/\\1/p'); do\n\
             \x20 eval \"val=\\${{$v}}\"\n\
             \x20 set -- \"$v=$val\" \"$@\"\n\
             done\n\
             exec env -i IN_SUDO=1 FAKE=\"$FAKE\" PATH=\"$given\" XDG_RUNTIME_DIR=/run/user/0 \"$@\"\n"
        ),
        "runuser" => format!(
            "echo \"$*\" >>\"$FAKE/runuser\"\n\
             echo \"${{XDG_RUNTIME_DIR-unset}}\" >>\"$FAKE/runuser.xdg\"\n\
             {says}\
             while [ \"$1\" != -- ]; do [ $# -gt 1 ] || exit 9; shift; done\n\
             shift\n\
             exec env PATH=\"$given\" \"$@\"\n"
        ),
        "systemctl" | "journalctl" => format!("echo \"$*\" >>\"$FAKE/{name}\"\n{says}"),
        "systemd-cat" => "[ ! -e \"$FAKE/fail-systemd-cat\" ] || exit 1\n\
             printf '%s ' \"$*\" >>\"$FAKE/journal\"\n\
             PATH=\"$given\" exec cat >>\"$FAKE/journal\"\n"
            .into(),
        "box-notify" => format!(
            "(IFS='|'; echo \"$*\") >>\"$FAKE/notify\"\n{says}[ ! -e \"$FAKE/fail-notify\" ]\n"
        ),
        "proj" => format!(
            "n=$(cat \"$FAKE/proj.n\" 2>/dev/null || echo 0)\n\
             n=$((n + 1))\n\
             echo \"$n\" >\"$FAKE/proj.n\"\n\
             {{ for w; do printf '%s\\0' \"$w\"; done; printf '\\036'; }} >>\"$FAKE/proj\"\n\
             if [ -e \"$FAKE/fail-proj\" ]; then\n\
             \x20 echo 'proj: no' >&2\n\
             \x20 rc=$(cat \"$FAKE/fail-proj\")\n\
             \x20 exit \"${{rc:-1}}\"\n\
             fi\n\
             {says}\
             echo \"job-$n\"\n"
        ),
        other => panic!("burrow_test has no stand-in for {other}"),
    };
    format!("{HEAD}{body}")
}

/// A new dir with the stand-ins for `names` (from [`OWNED`]) in it, readable
/// by anyone, so a run as nobody finds them too. Keep it in a `OnceLock` for
/// the test binary, and add the harness's own stand-ins to it with
/// [`crate::executable`].
pub fn stand_ins(names: &[&str]) -> tempfile::TempDir {
    let path = std::env::var("PATH").unwrap_or_default();
    assert!(!path.contains('\''), "PATH has a quote in it: {path}");
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        crate::executable(dir.path().join(name), script(name).replace("@PATH@", &path));
    }
    crate::permissions(dir.path(), 0o755);
    dir
}

/// Whether the tests run as root, so a run as nobody (65534) is a different
/// user's, as it is on the machine.
pub fn root() -> bool {
    rustix::process::geteuid().is_root()
}

/// `path` and everything under it nobody's, for a run as nobody.
pub fn chown(path: &Path) {
    std::os::unix::fs::lchown(path, Some(65534), Some(65534)).unwrap();
    if path.is_dir() && !path.is_symlink() {
        for entry in fs::read_dir(path).unwrap() {
            chown(&entry.unwrap().path());
        }
    }
}

/// How a run ended and what it said on stdout and stderr.
pub fn said(out: &Output) -> (Option<i32>, String, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The runs of the `proj` stand-in kept in `$FAKE/proj`, each its words.
pub fn proj_jobs(fake: &Path) -> Vec<Vec<String>> {
    let kept = fs::read(fake.join("proj")).unwrap_or_default();
    kept.split(|b| *b == 0x1e)
        .filter(|call| !call.is_empty())
        .map(|call| {
            let call = call.strip_suffix(&[0]).unwrap_or(call);
            call.split(|b| *b == 0)
                .map(|w| String::from_utf8_lossy(w).into_owned())
                .collect()
        })
        .collect()
}
