//! Each check's command runs in a fence of its own (burrow_process's
//! [`fence`](burrow_process::fence)), a cgroup under the run's unit's: once
//! the command has exited, what it left is killed and the run waits until
//! the fence is empty before its worktree goes.

use burrow_process::fence::empty;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The fences under a cgroup: `check-I` for the I-th check, entered
/// through `burrow-proof enter` ([`burrow_process::fence::enter`]), so
/// nothing of the command runs before it's inside.
pub struct Cgroups {
    pub dir: PathBuf,
    /// This program, which `enter`s.
    pub exe: PathBuf,
}

impl Cgroups {
    /// The fences under this process's own cgroup (its unit's).
    pub fn own(exe: PathBuf) -> Result<Cgroups, String> {
        Ok(Cgroups {
            dir: burrow_process::fence::own()?,
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
