//! A fence: a cgroup v2 of its own, under the caller's (its unit has
//! `Delegate=yes`, so its cgroup is the unit's to divide), that a command
//! is started in ([`enter`]). A process can leave its process group and
//! its session, close every descriptor and outlive its parent, but never
//! its cgroup: once the command is done with, what it left is killed
//! (`cgroup.kill`) and waited for until the cgroup is empty
//! (`cgroup.events`) ([`empty`]). A stop or a timeout of the unit ends all
//! of its cgroups at once, as systemd ends any unit's. burrow-proof fences
//! each check's command, the owner's door's broker each door.

use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

/// The cgroup v2 hierarchy, where systemd mounts it.
pub const MOUNT: &str = "/sys/fs/cgroup";

/// This process's own cgroup (its unit's), as /proc/self/cgroup names it
/// under [`MOUNT`].
pub fn own() -> Result<PathBuf, String> {
    let text =
        fs::read_to_string("/proc/self/cgroup").map_err(|e| format!("/proc/self/cgroup: {e}"))?;
    let dir = unified(&text)
        .ok_or_else(|| format!("/proc/self/cgroup names no cgroup v2 one: {}", text.trim()))?;
    Ok(Path::new(MOUNT).join(dir.trim_start_matches('/')))
}

/// The cgroup v2 line of /proc/self/cgroup (`0::/PATH`): PATH.
pub fn unified(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .filter(|p| p.starts_with('/'))
}

/// FENCE's processes all killed, waited for until the kernel says none is
/// left, and FENCE removed. None there: nothing to do. No deadline of its
/// own: the unit's (RuntimeMaxSec, or the stop's) bounds it.
pub fn empty(fence: &Path) -> io::Result<()> {
    if !fence.exists() {
        return Ok(());
    }
    fs::write(fence.join("cgroup.kill"), "1")?;
    while populated(fence)? {
        thread::sleep(Duration::from_millis(20));
    }
    fs::remove_dir(fence)
}

/// Whether FENCE or a cgroup under it holds a process.
pub fn populated(fence: &Path) -> io::Result<bool> {
    let events = fs::read_to_string(fence.join("cgroup.events"))?;
    match events.lines().find_map(|l| l.strip_prefix("populated ")) {
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(io::Error::other(format!(
            "{}: no populated line",
            fence.join("cgroup.events").display()
        ))),
    }
}

/// `PROGRAM enter FENCE -- COMMAND ARGS...`, as a program that fences
/// runs it: this process moved into FENCE, then replaced by COMMAND, so
/// nothing of COMMAND runs before it's inside. Only an error comes back.
pub fn enter(fence: &Path, program: &str, args: &[String]) -> io::Error {
    if let Err(e) = fs::write(fence.join("cgroup.procs"), "0") {
        return io::Error::new(
            e.kind(),
            format!("{}: {e}", fence.join("cgroup.procs").display()),
        );
    }
    Command::new(program).args(args).exec()
}
