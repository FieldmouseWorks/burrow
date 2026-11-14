//! burrow (modules/switch.nix): a user manager is re-executed only when it
//! runs another systemd than the new system's.
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
//! The re-execution is only needed to put the new systemd binary in place:
//! the reload that follows it re-reads user.conf and every unit file. So,
//! as for PID 1, the parent compares the binary the user manager runs
//! (/proc/PID/exe of user@UID.service's main process, which only root may
//! read) with the new system's, and tells the user activation whether to
//! re-execute. Anything it can't read re-executes, as before.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dbus::blocking::{stdintf::org_freedesktop_dbus::Properties, LocalConnection};

/// What the parent tells the user activation: "0" when its manager already
/// runs the new systemd.
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

/// Whether a manager running `running` must re-execute to run `new`.
pub fn needed(running: Option<&Path>, new: &Path) -> bool {
    running != Some(new)
}

/// ENV's value for a manager running `running`, the new systemd being `new`.
pub fn value(running: Option<&Path>, new: &Path) -> &'static str {
    if needed(running, new) {
        "1"
    } else {
        "0"
    }
}

/// In the user activation: whether to re-execute its manager. Only the
/// parent's "0" skips it.
pub fn wanted(value: Option<&str>) -> bool {
    value != Some("0")
}

#[cfg(test)]
mod tests;
