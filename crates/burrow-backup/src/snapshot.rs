//! A command run with each dataset's snapshot mounted read-only over its own
//! path, in a mount namespace of its own: restic sees the paths it saw last
//! time, so it reads only the files that changed since.
//!
//! What the command needs from the live box, whichever dataset it's in (the
//! state directory with restic's cache and rclone's config, the password, a
//! temp directory, and a directory of the drill's), is bound under
//! /run/HOST-backup-ns first, where no snapshot hides it, and found there.

use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::mounts::Mounts;
use crate::{Run, restic_env};

/// Where the namespace keeps what it needs from the live box.
pub fn ns(run: &Run) -> PathBuf {
    PathBuf::from(format!("/run/{}-ns", run.cfg.name))
}

/// `cmd` in a namespace with the snapshot `name` mounted, and `bind` (the
/// drill's directory) at ns/drill.
pub fn command(run: &Run, name: &str, bind: Option<&Path>, cmd: &[OsString]) -> Command {
    let mut c = run.command("unshare");
    c.args(["--mount", "--propagation", "private", "--"])
        .args(run.itself())
        .args(["__in-snapshot", name]);
    if let Some(dir) = bind {
        c.arg("--bind").arg(dir);
    }
    c.args(cmd);
    c
}

/// (In the namespace.) `__in-snapshot NAME [--bind DIR] CMD...`: mounts
/// the snapshot and becomes CMD.
pub fn enter(run: &Run, args: &[OsString]) -> String {
    match mount_all(run, args) {
        Ok(mut cmd) => {
            let e = cmd.exec();
            format!("{}: {e}", cmd.get_program().to_string_lossy())
        }
        Err(e) => e,
    }
}

fn mount_all(run: &Run, args: &[OsString]) -> Result<Command, String> {
    let (name, rest) = args.split_first().ok_or("__in-snapshot: no snapshot")?;
    let (bind, cmd) = match rest {
        [flag, dir, cmd @ ..] if flag == "--bind" => (Some(dir), cmd),
        cmd => (None, cmd),
    };
    let (program, cmd_args) = cmd.split_first().ok_or("__in-snapshot: no command")?;
    let name = name
        .to_str()
        .ok_or("__in-snapshot: a snapshot's name is text")?;
    let cfg = run.cfg;
    let mounts = Mounts::find(&cfg.datasets)?;
    let ns = ns(run);
    let dir = |d: &Path| fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()));
    dir(&ns)?;
    mount(run, &["-t", "tmpfs", "-o", "mode=700"], &cfg.name, &ns)?;
    for d in ["state", "tmp", "drill"] {
        dir(&ns.join(d))?;
    }
    let password = ns.join("password");
    File::create(&password).map_err(|e| format!("{}: {e}", password.display()))?;
    let tmp = std::env::var_os("TMPDIR")
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "/tmp".into());
    bind_mount(run, &cfg.state, &ns.join("state"))?;
    bind_mount(run, Path::new(&tmp), &ns.join("tmp"))?;
    bind_mount(run, &cfg.password_file, &password)?;
    if let Some(d) = bind {
        bind_mount(run, Path::new(d), &ns.join("drill"))?;
    }
    for (i, (ds, at)) in mounts.0.iter().enumerate() {
        // ZFS won't mount on a directory that isn't empty: on an empty one
        // first, then over the dataset's path.
        let on = ns.join((i + 1).to_string());
        dir(&on)?;
        mount(run, &["-t", "zfs", "-o", "ro"], format!("{ds}@{name}"), &on)?;
        bind_mount(run, &on, Path::new(at))?;
    }
    let mut c = run.command(program);
    c.args(cmd_args)
        .envs(restic_env(cfg, &ns.join("state"), &password))
        .env("TMPDIR", ns.join("tmp"));
    Ok(c)
}

fn bind_mount(run: &Run, from: &Path, to: &Path) -> Result<(), String> {
    mount(run, &["--bind"], from, to)
}

fn mount(
    run: &Run,
    how: &[&str],
    what: impl AsRef<std::ffi::OsStr>,
    on: &Path,
) -> Result<(), String> {
    let what = what.as_ref();
    let out = run
        .command("mount")
        .args(how)
        .arg(what)
        .arg(on)
        .output()
        .map_err(|e| format!("mount: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "mount {} {} {} failed ({}): {}",
        how.join(" "),
        what.to_string_lossy(),
        on.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim_end()
    ))
}
