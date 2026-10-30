//! A plan carried out on a server's cgroup.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;

use crate::cap::Cap;
use crate::place::{Unit, plan};
use crate::procs::{Pid, Table};

/// Linux's "no such process": one that ended before it could be moved.
const ESRCH: i32 = 3;

pub enum Applied {
    Done,
    /// A process started in the unit's own cgroup while the rest moved out,
    /// so it can't hand memory down yet: look again.
    Again,
}

/// Places a server's sessions, given its cgroup and what they are: each in
/// a group of its own, and the rest in `server`, every one under `cap`.
pub fn apply(dir: &Path, table: &Table, sessions: &[Pid], cap: Cap) -> io::Result<Applied> {
    let plan = plan(&read(dir)?, table, sessions);
    let on = memory_on(dir)?;
    for group in &plan.groups {
        let path = dir.join(group.name());
        match fs::create_dir(&path) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            r => r.map_err(at(&path))?,
        }
        // Capped before anything moves in, once there's a cap to set.
        if on {
            write(&path.join("memory.max"), &cap.memory_max())?;
        }
    }
    for (pid, group) in &plan.moves {
        let path = dir.join(group.name()).join("cgroup.procs");
        match fs::write(&path, pid.to_string()) {
            Err(e) if e.raw_os_error() == Some(ESRCH) => {}
            r => r.map_err(at(&path))?,
        }
    }
    if !on {
        // A cgroup with a process of its own can't hand a controller down.
        if !procs(dir)?.is_empty() {
            return Ok(Applied::Again);
        }
        let path = dir.join("cgroup.subtree_control");
        match fs::write(&path, "+memory") {
            Err(e) if e.kind() == ErrorKind::ResourceBusy => return Ok(Applied::Again),
            r => r.map_err(at(&path))?,
        }
        for group in &plan.groups {
            write(
                &dir.join(group.name()).join("memory.max"),
                &cap.memory_max(),
            )?;
        }
    }
    for group in &plan.ended {
        // Still busy: a tool its session left running, gone by a later scan.
        let path = dir.join(group.name());
        match fs::remove_dir(&path) {
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ResourceBusy) => {}
            r => r.map_err(at(&path))?,
        }
    }
    Ok(Applied::Done)
}

/// An error that names the file it came from.
fn at(path: &Path) -> impl FnOnce(io::Error) -> io::Error + '_ {
    move |e| io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

fn write(path: &Path, value: &str) -> io::Result<()> {
    fs::write(path, value).map_err(at(path))
}

/// What a server's cgroup holds now.
fn read(dir: &Path) -> io::Result<Unit> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(dir).map_err(at(dir))? {
        let entry = entry.map_err(at(dir))?;
        if entry.file_type().map_err(at(dir))?.is_dir() {
            groups.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    groups.sort();
    let server = match procs(&dir.join("server")) {
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        r => r?,
    };
    Ok(Unit {
        root: procs(dir)?,
        server,
        groups,
    })
}

fn procs(dir: &Path) -> io::Result<Vec<Pid>> {
    let path = dir.join("cgroup.procs");
    let procs = fs::read_to_string(&path).map_err(at(&path))?;
    Ok(procs
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect())
}

/// Whether the unit hands memory down to its groups yet.
fn memory_on(dir: &Path) -> io::Result<bool> {
    let path = dir.join("cgroup.subtree_control");
    let control = fs::read_to_string(&path).map_err(at(&path))?;
    Ok(control.split_ascii_whitespace().any(|c| c == "memory"))
}
