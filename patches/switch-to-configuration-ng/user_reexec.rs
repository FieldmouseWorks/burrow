//! burrow (modules/switch.nix): a user manager is re-executed only when the
//! switch can show it already runs what a re-execution would give it.
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
//! A re-execution gives a manager two things a reload doesn't: the new
//! systemd binary, and the settings of its config only a starting manager
//! applies (`initialize_runtime`: TimerSlackNSec, SystemCallArchitectures).
//! So the parent, as root, skips it only when both are known to be in place:
//!
//! - the binary: /proc/PID/exe of user@UID.service's main process is the new
//!   system's systemd;
//! - the config: a fingerprint of every source systemd reads it from (the
//!   user's own user.conf and user.conf.d, found from the manager process's
//!   own HOME and XDG_CONFIG_HOME as systemd finds them, and /etc's, /run's,
//!   /usr/local/lib's and /usr/lib's) equals the baseline recorded when this
//!   same manager process last re-executed on this switch's say.
//!
//! The baseline is written only once a re-execution was seen to happen (the
//! private socket was bound again) and the user activation succeeded; a
//! failed or interrupted one removes it. It lives under /run, so a boot
//! starts with none: a manager re-executes once, then only on a change.
//! Anything that can't be read re-executes, as before. The fingerprint never
//! opens anything but a regular file of bounded size, without blocking.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dbus::blocking::{stdintf::org_freedesktop_dbus::Properties, LocalConnection};

/// What the parent tells the user activation: "0" when its manager can be
/// left running.
pub const ENV: &str = "__BURROW_USER_MANAGER_REEXEC";

/// Where each manager's baseline is kept, a file per uid.
pub const STATE: &str = "/run/nixos/burrow-user-reexec";

/// The most a config file, a directory listing or a process environment may
/// hold before the fingerprint gives up (and the manager re-executes).
const MAX_BYTES: u64 = 1 << 20;
const MAX_ENTRIES: usize = 4096;

/// The config sources systemd reads besides the user's own (CONF_PATHS).
pub const SYSTEM_DIRS: [&str; 4] = ["/etc/systemd", "/run/systemd", "/usr/local/lib/systemd", "/usr/lib/systemd"];

/// A running user manager, as root sees it.
pub struct Manager {
    pub pid: u32,
    pub binary: PathBuf,
    /// $XDG_CONFIG_HOME/systemd, from the process's own environment.
    pub config: Option<PathBuf>,
}

/// user@UID.service's main process, if it can be read.
pub fn manager(conn: &LocalConnection, uid: u32) -> Option<Manager> {
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
    let binary = Path::new(&format!("/proc/{pid}/exe")).canonicalize().ok()?;
    let mut environ = Vec::new();
    File::open(format!("/proc/{pid}/environ"))
        .ok()?
        .take(MAX_BYTES)
        .read_to_end(&mut environ)
        .ok()?;
    Some(Manager { pid, binary, config: config_dir(&environ) })
}

/// $XDG_CONFIG_HOME/systemd as systemd's sd_path_lookup finds it from a
/// NUL-separated environment: XDG_CONFIG_HOME if absolute, else
/// $HOME/.config if HOME is absolute. None otherwise: where systemd looks
/// then (the passwd entry) isn't followed here.
pub fn config_dir(environ: &[u8]) -> Option<PathBuf> {
    let get = |key: &str| {
        environ.split(|b| *b == 0).find_map(|kv| {
            let kv = std::str::from_utf8(kv).ok()?;
            let value = kv.strip_prefix(key)?.strip_prefix('=')?;
            Some(PathBuf::from(value)).filter(|p| p.is_absolute())
        })
    };
    get("XDG_CONFIG_HOME")
        .or_else(|| get("HOME").map(|home| home.join(".config")))
        .map(|config| config.join("systemd"))
}

/// One path, as the fingerprint has it: what it is, without opening
/// anything but a bounded regular file. None when it can't be told.
fn describe(path: &Path) -> Option<String> {
    let link = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some("absent".into()),
        Err(_) => return None,
    };
    let mut out = String::new();
    if link.file_type().is_symlink() {
        out += &format!("symlink {:?} -> ", fs::read_link(path).ok()?);
    }
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(out + "dangling"),
        Err(_) => return None,
    };
    let kind = meta.file_type();
    if kind.is_file() {
        if meta.len() > MAX_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits() | nix::fcntl::OFlag::O_NOCTTY.bits())
            .open(path)
            .ok()?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        out += &format!("file {hex}");
    } else if kind.is_dir() {
        out += "dir";
    } else if kind.is_char_device() || kind.is_block_device() {
        out += &format!("device {} {}", kind.is_char_device(), meta.rdev());
    } else if kind.is_fifo() {
        out += "fifo";
    } else if kind.is_socket() {
        out += "socket";
    } else {
        return None;
    }
    Some(out)
}

/// Every source of a user manager's config, `user` being its user's own
/// $XDG_CONFIG_HOME/systemd and `system` the other directories systemd
/// reads, in its order: each user.conf (the user's, then /etc's), then each
/// user.conf.d and every entry in it, whatever it is. None when any can't
/// be told.
pub fn fingerprint(user: &Path, system: &[&Path]) -> Option<String> {
    let mut out = String::new();
    for file in [user.join("user.conf"), system.first()?.join("user.conf")] {
        out += &format!("{file:?} {}\n", describe(&file)?);
    }
    for dir in std::iter::once(user).chain(system.iter().copied()) {
        let dir = dir.join("user.conf.d");
        let what = describe(&dir)?;
        out += &format!("{dir:?} {what}\n");
        if what != "dir" && !what.ends_with("-> dir") {
            continue;
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).ok()? {
            names.push(entry.ok()?.file_name());
            if names.len() > MAX_ENTRIES {
                return None;
            }
        }
        names.sort();
        for name in names {
            let path = dir.join(&name);
            out += &format!("{path:?} {}\n", describe(&path)?);
        }
    }
    Some(out)
}

/// What a baseline records for a manager: its process, its binary and its
/// config's fingerprint. None when the config can't be told.
pub fn state(manager: &Manager) -> Option<String> {
    let system: Vec<&Path> = SYSTEM_DIRS.iter().map(Path::new).collect();
    let config = fingerprint(manager.config.as_deref()?, &system)?;
    Some(format!("pid {}\nbinary {:?}\n{config}", manager.pid, manager.binary))
}

/// Whether a manager must re-execute: unless it runs the new binary and its
/// state is the baseline recorded at its last re-execution.
pub fn needed(binary: Option<&Path>, new: &Path, now: Option<&str>, baseline: Option<&str>) -> bool {
    binary != Some(new) || now.is_none() || now != baseline
}

pub fn baseline_path(uid: u32) -> PathBuf {
    Path::new(STATE).join(uid.to_string())
}

pub fn read_baseline(uid: u32) -> Option<String> {
    let path = baseline_path(uid);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() || meta.len() > 4 * MAX_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Records `state` as uid's baseline, or removes it (None). Best effort: a
/// baseline that isn't there only costs a re-execution.
pub fn write_baseline(uid: u32, state: Option<&str>) {
    let path = baseline_path(uid);
    let Some(state) = state else {
        let _ = fs::remove_file(path);
        return;
    };
    let tmp = path.with_extension("tmp");
    let written = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(STATE)
        .and_then(|_| fs::write(&tmp, state))
        .and_then(|_| fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = fs::remove_file(tmp);
        let _ = fs::remove_file(path);
    }
}

/// The private socket's inode: a re-execution binds a new one.
pub fn socket(runtime_dir: &str) -> Option<u64> {
    fs::symlink_metadata(Path::new(runtime_dir).join("systemd/private"))
        .ok()
        .map(|meta| meta.ino())
}

/// Whether the private socket was bound again since it was `before`, waiting
/// up to 10s for the manager to come back.
pub fn rebound(runtime_dir: &str, before: Option<u64>) -> bool {
    if before.is_none() {
        return false;
    }
    for _ in 0..100 {
        let now = socket(runtime_dir);
        if now.is_some() && now != before {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// In the user activation: whether to re-execute its manager. Only the
/// parent's "0" skips it.
pub fn wanted(value: Option<&str>) -> bool {
    value != Some("0")
}


#[cfg(test)]
mod tests;
