//! Each check's command runs in a cgroup of its own, a child of the run's
//! unit's (the unit has `Delegate=yes`, so its cgroup is the run's to
//! divide). A process can leave its process group and its session, close
//! every descriptor and outlive its parent, but never its cgroup: once the
//! command has exited, what it left is killed (`cgroup.kill`) and the run
//! waits until the cgroup is empty (`cgroup.events`) before its worktree
//! goes. A stop or a timeout of the unit ends all of its cgroups at once,
//! as systemd ends any unit's.

use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Where a check's command runs, and how what it leaves is ended.
pub trait Fence {
    /// A command line that runs PROGRAM ARGS inside the I-th check's fence,
    /// which is made now.
    fn around(&mut self, i: usize, program: &Path, args: &[&str]) -> io::Result<Command>;
    /// Every process left in the I-th check's fence ended, and the fence
    /// gone once none is.
    fn clear(&mut self, i: usize) -> io::Result<()>;
    /// What the run's folder keeps of where its fences are (`cgroup`), for
    /// `end` and other runs' sweeps (trees.rs).
    fn record(&self) -> Option<PathBuf>;
}

/// The cgroup v2 hierarchy, where systemd mounts it.
pub const MOUNT: &str = "/sys/fs/cgroup";

/// The fences under a cgroup: `check-I` for the I-th check, entered
/// through `burrow-proof enter` ([`enter`]), so nothing of the command
/// runs before it's inside.
pub struct Cgroups {
    pub dir: PathBuf,
    /// This program, which `enter`s.
    pub exe: PathBuf,
}

impl Cgroups {
    /// The fences under this process's own cgroup (its unit's), as
    /// /proc/self/cgroup names it under [`MOUNT`].
    pub fn own(exe: PathBuf) -> Result<Cgroups, String> {
        let text = fs::read_to_string("/proc/self/cgroup")
            .map_err(|e| format!("/proc/self/cgroup: {e}"))?;
        let dir = unified(&text)
            .ok_or_else(|| format!("/proc/self/cgroup names no cgroup v2 one: {}", text.trim()))?;
        Ok(Cgroups {
            dir: Path::new(MOUNT).join(dir.trim_start_matches('/')),
            exe,
        })
    }

    fn check(&self, i: usize) -> PathBuf {
        self.dir.join(format!("check-{i}"))
    }
}

/// Every check's fence still under DIR cleared (`end`, once systemd has
/// ended the unit's processes). DIR gone: nothing left to clear.
pub fn clear_all(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("check-"))
        {
            empty(&entry.path())?;
        }
    }
    Ok(())
}

/// The cgroup v2 line of /proc/self/cgroup (`0::/PATH`): PATH.
pub fn unified(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .filter(|p| p.starts_with('/'))
}

impl Fence for Cgroups {
    fn around(&mut self, i: usize, program: &Path, args: &[&str]) -> io::Result<Command> {
        let fence = self.check(i);
        fs::create_dir(&fence)?;
        let mut c = Command::new(&self.exe);
        c.arg("enter").arg(&fence).arg("--").arg(program).args(args);
        Ok(c)
    }

    fn clear(&mut self, i: usize) -> io::Result<()> {
        empty(&self.check(i))
    }

    fn record(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
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

/// `burrow-proof enter FENCE -- PROGRAM ARGS...`: this process moved into
/// FENCE, then replaced by PROGRAM. Only an error comes back.
pub fn enter(fence: &Path, program: &str, args: &[String]) -> io::Error {
    if let Err(e) = fs::write(fence.join("cgroup.procs"), "0") {
        return io::Error::new(
            e.kind(),
            format!("{}: {e}", fence.join("cgroup.procs").display()),
        );
    }
    Command::new(program).args(args).exec()
}
