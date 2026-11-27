//! Removing every entry proj set aside in SCRATCH/jobs, as root, by file
//! descriptor alone. The job's user can write all of SCRATCH/jobs while this
//! runs, so no name in it is trusted: each step opens one component below a
//! descriptor already held, following no symlink and crossing no mount
//! (`RESOLVE_NO_XDEV` refuses every mount point, a bind mount of the same
//! filesystem included), and nothing is ever unmounted. A symlink is removed
//! as a link. What can't be removed whole is renamed out of the watched name,
//! so one stuck entry never holds up the rest or the path unit.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rfs, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, ResolveFlags};
use rustix::io::Errno;

use crate::{GONE, STUCK};

/// How deep a tree it goes: each level holds a descriptor, and the job's user
/// decides the depth. Deeper is stuck. The unit's LimitNOFILE is above it.
pub const DEPTH: usize = 4096;

/// Below SCRATCH/jobs: one component at a time, never through a link or a
/// mount.
const BELOW: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_XDEV);

/// What a sweep did, by name in SCRATCH/jobs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Removed whole.
    pub removed: Vec<OsString>,
    /// Set aside entries it couldn't remove, by the stuck name each now has,
    /// and why.
    pub stuck: Vec<(OsString, String)>,
    /// Stuck entries from an earlier sweep still there, and why.
    pub still: Vec<(OsString, String)>,
    /// Set aside entries it could neither remove nor rename (one that is a
    /// mount point itself), still under the watched name, and why.
    pub held: Vec<(OsString, String)>,
}

impl Sweep {
    /// Whether the sweep failed: it left a watched name behind, which nothing
    /// will ever notice (the notices read only stuck names), so its unit
    /// fails and the machine's failed-unit watchdog takes it. A stuck entry
    /// isn't a failure: it's out of the watched name, and noticed.
    pub fn failed(&self) -> bool {
        !self.held.is_empty()
    }
}

/// Removes each direct child of `jobs` whose name starts with [`GONE`] or
/// [`STUCK`]; renames a [`GONE`] one it can't remove to [`STUCK`]. Fails only
/// when `jobs` itself can't be opened or read.
pub fn sweep(jobs: &Path) -> io::Result<Sweep> {
    let jobs = open_jobs(jobs)?;
    let mut names: Vec<OsString> = entries(&jobs)?
        .into_iter()
        .filter(|n| {
            n.as_bytes().starts_with(GONE.as_bytes()) || n.as_bytes().starts_with(STUCK.as_bytes())
        })
        .collect();
    names.sort();
    let mut done = Sweep::default();
    for name in names {
        let Err(why) = remove(&jobs, &name, Path::new(&name), 0) else {
            done.removed.push(name);
            continue;
        };
        let Some(rest) = name.as_bytes().strip_prefix(GONE.as_bytes()) else {
            done.still.push((name, why));
            continue;
        };
        match set_stuck(&jobs, &name, rest) {
            Ok(to) => done.stuck.push((to, why)),
            Err(e) => done
                .held
                .push((name, format!("{why}; and it can't be renamed: {e}"))),
        }
    }
    Ok(done)
}

/// SCRATCH/jobs, through no symlink. It may be a mount point itself (the
/// unit's ReadWritePaths makes it one), so mounts aren't refused on the way to
/// it, only below it.
fn open_jobs(jobs: &Path) -> io::Result<OwnedFd> {
    if jobs.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::other("the jobs dir's path can't go up"));
    }
    let base = rfs::open(
        if jobs.is_absolute() { "/" } else { "." },
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let relative = jobs.strip_prefix("/").unwrap_or(jobs);
    Ok(rfs::openat2(
        &base,
        relative,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )?)
}

/// The names in an open directory, but . and ..
fn entries(dir: &OwnedFd) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(dir)? {
        let name = entry?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(&name).to_owned());
        }
    }
    Ok(names)
}

/// Removes `name` below `parent` and all of it, removing what it can of a tree
/// it can't remove whole. `at` is its path from SCRATCH/jobs, for what's said.
/// The first thing that stopped it is the error.
fn remove(parent: &OwnedFd, name: &OsStr, at: &Path, depth: usize) -> Result<(), String> {
    let said = |e: Errno| format!("{}: {}", at.display(), io::Error::from(e));
    let gone = |r: Result<(), Errno>| match r {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(said(e)),
    };
    // Pinned as it is, a link or a mount point without going through it.
    let pinned = match rfs::openat2(
        parent,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        BELOW,
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(()),
        Err(Errno::XDEV) => return Err(format!("{}: a mount point, left as it is", at.display())),
        Err(e) => return Err(said(e)),
    };
    let kind = rfs::fstat(&pinned).map_err(said)?;
    // Its type is all it's for: a level holds one descriptor, the dir's below.
    drop(pinned);
    if FileType::from_raw_mode(kind.st_mode) != FileType::Directory {
        return gone(rfs::unlinkat(parent, name, AtFlags::empty()));
    }
    if depth >= DEPTH {
        return Err(format!("{}: deeper than {DEPTH} directories", at.display()));
    }
    let dir = match rfs::openat2(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        BELOW,
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(()),
        Err(Errno::XDEV) => return Err(format!("{}: a mount point, left as it is", at.display())),
        Err(e) => return Err(said(e)),
    };
    let mut first = None;
    match entries(&dir) {
        Ok(names) => {
            for child in names {
                if let Err(e) = remove(&dir, &child, &at.join(&child), depth + 1) {
                    first.get_or_insert(e);
                }
            }
        }
        Err(e) => {
            first = Some(format!("{}: {e}", at.display()));
        }
    }
    drop(dir);
    match gone(rfs::unlinkat(parent, name, AtFlags::REMOVEDIR)) {
        Ok(()) => Ok(()),
        Err(e) => Err(first.unwrap_or(e)),
    }
}

/// Renames set aside entry `name` (the watched prefix, then `rest`) to the
/// stuck prefix and `rest`, or with a number after it if that's taken.
fn set_stuck(jobs: &OwnedFd, name: &OsStr, rest: &[u8]) -> io::Result<OsString> {
    let mut to = PathBuf::from(OsStr::from_bytes(&[STUCK.as_bytes(), rest].concat()));
    for n in 1..=100 {
        match rfs::renameat_with(jobs, name, jobs.as_fd(), &to, RenameFlags::NOREPLACE) {
            Ok(()) => return Ok(to.into_os_string()),
            Err(Errno::EXIST) => {
                let mut next = OsString::from(STUCK);
                next.push(OsStr::from_bytes(rest));
                next.push(format!(".{n}"));
                to = next.into();
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::other("a hundred stuck names like it are taken"))
}

#[cfg(test)]
mod tests;
