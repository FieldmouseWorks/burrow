//! burrow (modules/switch.nix): a user manager is re-executed only when the
//! switch changes what only a re-execution applies.
//!
//! switch-to-configuration re-executes every user manager on every switch.
//! A manager closes its private socket ($XDG_RUNTIME_DIR/systemd/private)
//! when it re-executes and binds a new one once it has deserialized, and
//! `systemctl --user` and `systemd-run --user` reach it only through that
//! socket, so each call in that window fails with "Failed to connect to user
//! scope bus via local transport: Connection refused". A machine whose
//! pipeline runs as user units (the steward's proofs, proj's jobs, the
//! tests they run) loses whatever such a call was for, at every switch.
//!
//! The reload that follows re-reads the unit files and user.conf, but a few
//! of user.conf's settings (TimerSlackNSec, SystemCallArchitectures) only
//! take effect in a starting manager (systemd's `initialize_runtime`). So a
//! manager is re-executed when any of these holds, as PID 1 is for its
//! binary and system.conf:
//!
//! - it runs another systemd binary than the new system's: the parent
//!   compares /proc/PID/exe of user@UID.service's main process, which only
//!   root may read;
//! - the new system's /etc/systemd/user.conf or user.conf.d differs from the
//!   old one's;
//! - its user has a user.conf or user.conf.d of their own (under
//!   $XDG_CONFIG_HOME/systemd), or where that is can't be told: no switch
//!   knows whether those changed, so they keep the re-execution they had.
//!
//! Anything that can't be read re-executes, as before.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dbus::blocking::{stdintf::org_freedesktop_dbus::Properties, LocalConnection};

/// What the parent tells the user activation: "0" when its manager already
/// runs the new systemd and the system's user.conf didn't change.
pub const ENV: &str = "__BURROW_USER_MANAGER_REEXEC";

/// The binary user@UID.service's main process runs, if it can be read.
pub fn manager_binary(conn: &LocalConnection, uid: u32) -> Option<PathBuf> {
    let timeout = Duration::from_millis(5000);
    let (unit,): (dbus::Path<'static>,) = conn
        .with_proxy("org.freedesktop.systemd1", "/org/freedesktop/systemd1", timeout)
        .method_call(
            "org.freedesktop.systemd1.Manager",
            "GetUnit",
            (format!("user@{uid}.service"),),
        )
        .ok()?;
    let pid: u32 = conn
        .with_proxy("org.freedesktop.systemd1", unit, timeout)
        .get("org.freedesktop.systemd1.Service", "MainPID")
        .ok()?;
    if pid == 0 {
        return None;
    }
    Path::new(&format!("/proc/{pid}/exe")).canonicalize().ok()
}

/// A manager config, `user.conf` and its `user.conf.d` drop-ins, under a
/// `systemd` directory: each file's name and bytes, in order. None when a
/// file that is there can't be read.
fn config(systemd: &Path) -> Option<Vec<(String, Vec<u8>)>> {
    let read = |path: &Path| match std::fs::read(path) {
        Ok(bytes) => Some(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    };
    let mut files = Vec::new();
    if let Some(bytes) = read(&systemd.join("user.conf"))? {
        files.push(("user.conf".to_string(), bytes));
    }
    let dir = systemd.join("user.conf.d");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(files),
        Err(_) => return None,
    };
    let mut names = Vec::new();
    for entry in entries {
        names.push(entry.ok()?.file_name().into_string().ok()?);
    }
    names.sort();
    for name in names {
        let bytes = read(&dir.join(&name))??;
        files.push((format!("user.conf.d/{name}"), bytes));
    }
    Some(files)
}

/// Whether the system's user manager config differs between two systems'
/// `etc/systemd` directories (or either can't be read).
pub fn config_changed(old: &Path, new: &Path) -> bool {
    match (config(old), config(new)) {
        (Some(old), Some(new)) => old != new,
        _ => true,
    }
}

/// Whether a user has a manager config of their own under `systemd`
/// ($XDG_CONFIG_HOME/systemd), or where that is can't be told (None).
pub fn own_config(systemd: Option<&Path>) -> bool {
    match systemd {
        None => true,
        Some(dir) => config(dir).is_none_or(|files| !files.is_empty()),
    }
}

/// ENV's value for a manager running `running`, the new systemd being `new`,
/// the system's user manager config changed or not.
pub fn value(running: Option<&Path>, new: &Path, config_changed: bool) -> &'static str {
    if running != Some(new) || config_changed {
        "1"
    } else {
        "0"
    }
}

/// In the user activation: whether to re-execute its manager. Only the
/// parent's "0", for a user with no manager config of their own, skips it.
pub fn wanted(value: Option<&str>, own_config: bool) -> bool {
    value != Some("0") || own_config
}

#[cfg(test)]
mod tests;
