//! `burrow-proof start DIR`: the run DIR's record holds, started in its
//! unit, a transient one of the machine user's own manager that goes once
//! it's over (`--collect`):
//!
//! - `RuntimeMaxSec`, the record's timeout: the whole run, its sweep, fetch
//!   and worktrees as much as its commands, ends there (the unit's result
//!   is then `timeout`);
//! - `Delegate=yes`: its cgroup is the run's to divide, a fence for each
//!   command (fence.rs);
//! - `ExecStopPost`, `burrow-proof end DIR`: however it ends, once systemd
//!   has ended every process of the unit (SIGTERM, then SIGKILL past its
//!   stop timeout, the user manager's default), its worktrees go and
//!   `end.json` says how it ended.
//!
//! Its environment is the manager's own, nothing of the steward's.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Record, plain};

/// systemd-run's arguments for the run whose folder is DIR, EXE being this
/// program. Neither line expands `$` (`:`, --expand-environment=no).
pub fn argv(exe: &str, dir: &str, rec: &Record) -> Vec<String> {
    vec![
        "--user".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit={}", rec.unit),
        "--property=Delegate=yes".into(),
        format!("--property=RuntimeMaxSec={}s", rec.timeout),
        format!("--property=ExecStopPost=:{exe} end {dir}"),
        "--expand-environment=no".into(),
        "--".into(),
        exe.into(),
        "run".into(),
        dir.into(),
    ]
}

/// The run whose folder is DIR started by SYSTEMD_RUN, EXE being this
/// program, or why not: a record it can't run, or systemd's refusal.
pub fn start(dir: &Path, exe: &Path, systemd_run: &Path) -> Result<(), String> {
    let rec = Record::read(dir)?;
    let word = |p: &Path| {
        p.to_str()
            .filter(|s| p.is_absolute() && s.bytes().all(plain))
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "{} isn't a path a unit's command line takes as it is",
                    p.display()
                )
            })
    };
    let (exe, dir) = (word(exe)?, word(dir)?);
    let mut c = Command::new(systemd_run);
    c.args(argv(&exe, &dir, &rec)).stdin(Stdio::null());
    // The user's manager, from a unit that doesn't say where it is.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let uid = fs::metadata("/proc/self")
            .map_err(|e| format!("/proc/self: {e}"))?
            .uid();
        c.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    let out = c
        .output()
        .map_err(|e| format!("{}: {e}", systemd_run.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!("systemd-run refused {}: {}", rec.unit, said.trim()))
}
