//! The flow's state (Conary's `CONARY_DATA`, its docs/flow.md "State and
//! workers": each PR's memory, the stall clocks, the fix rounds, a pause,
//! conaryd's queue), carried to whoever runs the flow now
//! (modules/ledger.nix, `burrow.ledger.flow.data`). The flow keeps it in
//! the ledger's `data` with no controller and in a directory of the
//! controller's own with one, so turning the controller on, changing it or
//! turning it off moves it, and without this the flow would start over on
//! the first pass after the switch.
//!
//! [`carry`] runs as root before any of the flow's units, on each boot and
//! at a switch that changes who runs the flow. The state is in one place
//! or the other: where it's kept now (`data`), or where it was kept
//! (`other`). Found in `other`, it's copied into `data` and `other` is
//! put aside and removed; found in `data` with files someone else owns (a
//! controller changed), it's copied in place; found in `data` all its
//! owner's, only the directory is narrowed to 0700. Found in both, it's
//! refused: two
//! histories aren't merged. An empty directory (what tmpfiles or a unit
//! made) holds no state.
//!
//! A copy, never a rename and a chown: the old user may still hold a
//! descriptor of a file it wrote, or a hard link to one, and would keep
//! writing the new user's state through it. Every file is made afresh, the
//! new user's, in a directory only root may enter until it's whole, then
//! put in place at once. Nothing follows a link: each entry is opened
//! once, without following one, and read through that descriptor alone.
//! What isn't a directory or a file with one link (a link, a socket, a
//! file linked from elsewhere) isn't the flow's, and is refused. So is a
//! file a process holds a lock on: one of the flow's own (a worker
//! outlives conaryd) still running as the old user.
use rustix::fs::{
    self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, Timespec, Timestamps,
};
use rustix::io::Errno;
use rustix::process::{Gid, Uid};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Where a copy is built, beside where it goes, until it's whole.
const STAGING: &CStr = c".burrow-flow-carry";

/// Whose the state is now.
#[derive(Clone, Copy, Debug)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

/// What [`carry`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Carried {
    /// The state was where it's kept, its owner's already.
    Kept,
    /// There was none: an empty directory now waits for it.
    Made,
    /// Copied from where it was kept before.
    Moved,
    /// Copied in place, to its new owner.
    Handed,
}

/// Why [`carry`] did nothing.
#[derive(Debug)]
pub enum Refused {
    /// A file in the state is locked: a process of the flow's still runs
    /// as the user it had. Again once it has exited.
    Busy(String),
    /// Both places hold state, or one holds what the flow never writes.
    Refused(String),
    /// The file system said no.
    Failed(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Busy(m) | Refused::Refused(m) | Refused::Failed(m) => f.write_str(m),
        }
    }
}

fn failed(what: &str) -> impl Fn(Errno) -> Refused + '_ {
    move |e| Refused::Failed(format!("{what}: {e}"))
}

/// A path's directory, opened without following a link as its last
/// component, and its last component; `None` with no such directory.
fn place(path: &Path) -> Result<Option<(OwnedFd, CString)>, Refused> {
    let shown = path.display().to_string();
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Refused::Refused(format!(
            "{shown}: not a directory's entry"
        )));
    };
    let parent = match fs::open(parent, dir_flags(), Mode::empty()) {
        Ok(parent) => parent,
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(failed(&shown)(e)),
    };
    let name =
        CString::new(name.as_bytes()).map_err(|e| Refused::Refused(format!("{shown}: {e}")))?;
    Ok(Some((parent, name)))
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[derive(Debug, PartialEq, Eq)]
enum Found {
    Absent,
    Empty,
    State,
    /// Not a directory: a link, or anything else.
    Other,
}

fn look(parent: &OwnedFd, name: &CStr) -> Result<Found, Errno> {
    let dir = match fs::openat(parent, name, dir_flags(), Mode::empty()) {
        Ok(dir) => dir,
        Err(Errno::NOENT) => return Ok(Found::Absent),
        Err(Errno::LOOP | Errno::NOTDIR) => return Ok(Found::Other),
        Err(e) => return Err(e),
    };
    for entry in fs::Dir::read_from(&dir)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(Found::State);
        }
    }
    Ok(Found::Empty)
}

/// Carry the flow's state into `data` from `other`, or hand it in place,
/// to `owner`, as the module's docs say.
pub fn carry(data: &Path, other: &Path, owner: Owner) -> Result<Carried, Refused> {
    let shown = data.display().to_string();
    let Some((dp, dn)) = place(data)? else {
        return Err(Refused::Failed(format!(
            "{shown}: its directory isn't there"
        )));
    };
    let new = look(&dp, &dn).map_err(failed(&shown))?;
    if new == Found::Other {
        return Err(Refused::Refused(format!(
            "{shown}: not a directory, so not the flow's state"
        )));
    }
    // Where it was kept may be gone with its directory: the ledger's, say.
    let from = other.display().to_string();
    if let Some((op, on)) = place(other)? {
        match (look(&op, &on).map_err(failed(&from))?, &new) {
            (Found::State, Found::State) => {
                return Err(Refused::Refused(format!(
                    "{shown} and {from} both hold the flow's state: two histories, which aren't merged. \
                     Move aside the one the flow shouldn't carry on from, and start this again."
                )));
            }
            (Found::State, _) => {
                copy(&op, &on, &dp, &dn, owner, &from)?;
                // Out of the way at once, and that flushed, so neither a
                // crash nor a removal cut short leaves a second history:
                // what's left is a copy's to clear.
                let aside = match remove(&op, STAGING) {
                    Ok(()) | Err(Errno::NOENT) => {
                        fs::renameat_with(&op, &on, &op, STAGING, RenameFlags::NOREPLACE)
                    }
                    Err(e) => Err(e),
                };
                aside
                    .and_then(|()| durable(&op))
                    .and_then(|()| remove(&op, STAGING))
                    .map_err(failed(&from))?;
                return Ok(Carried::Moved);
            }
            (Found::Other, Found::Absent | Found::Empty) => {
                return Err(Refused::Refused(format!(
                    "{from}: not a directory, so not the flow's state, and {shown} holds none"
                )));
            }
            (Found::Empty, _) => {
                fs::unlinkat(&op, on.as_c_str(), AtFlags::REMOVEDIR).map_err(failed(&from))?;
            }
            _ => {}
        }
    }
    if new == Found::Absent {
        fs::mkdirat(&dp, dn.as_c_str(), Mode::from_raw_mode(0o700)).map_err(failed(&shown))?;
        let dir =
            fs::openat(&dp, dn.as_c_str(), dir_flags(), Mode::empty()).map_err(failed(&shown))?;
        own(&dir, owner, 0o700).map_err(failed(&shown))?;
        return Ok(Carried::Made);
    }
    if owned(&dp, &dn, owner).map_err(failed(&shown))? {
        return Ok(Carried::Kept);
    }
    copy(&dp, &dn, &dp, &dn, owner, &shown)?;
    Ok(Carried::Handed)
}

/// Whether the directory `name` in `parent`, and all it holds, is
/// `owner`'s, made 0700, its group's and with no ACL if it isn't.
fn owned(parent: &OwnedFd, name: &CStr, owner: Owner) -> Result<bool, Errno> {
    let dir = fs::openat(parent, name, dir_flags(), Mode::empty())?;
    if !all_owned(&dir, owner.uid)? {
        return Ok(false);
    }
    // All of it the owner's already (the ledger's `data` as the flow kept
    // it before this ran, 0755): only the directory is narrowed, which
    // leaves what it holds reachable by its owner alone.
    let st = fs::fstat(&dir)?;
    if st.st_mode & 0o7777 != 0o700 || st.st_gid != owner.gid || acl(&dir)? {
        own(&dir, owner, 0o700)?;
    }
    Ok(true)
}

/// Whether `dir` and all it holds is `uid`'s.
fn all_owned(dir: &OwnedFd, uid: u32) -> Result<bool, Errno> {
    if fs::fstat(dir)?.st_uid != uid {
        return Ok(false);
    }
    for entry in fs::Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let p = match fs::openat(dir, name, flags, Mode::empty()) {
            Ok(p) => p,
            Err(Errno::NOENT) => continue,
            Err(e) => return Err(e),
        };
        let st = fs::fstat(&p)?;
        if st.st_uid != uid {
            return Ok(false);
        }
        if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
            let d = fs::openat(&p, c".", dir_flags(), Mode::empty())?;
            if !all_owned(&d, uid)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

const ACCESS: &CStr = c"system.posix_acl_access";
const DEFAULT: &CStr = c"system.posix_acl_default";

/// Whether the directory has an ACL of its own.
fn acl(dir: &OwnedFd) -> Result<bool, Errno> {
    for name in [ACCESS, DEFAULT] {
        match fs::fgetxattr(dir, name, &mut [0u8; 0]) {
            Ok(_) => return Ok(true),
            Err(Errno::NODATA | Errno::OPNOTSUPP) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// Make `dir` `owner`'s, with `mode` and no ACL.
fn own(dir: &OwnedFd, owner: Owner, mode: u32) -> Result<(), Errno> {
    for name in [ACCESS, DEFAULT] {
        match fs::fremovexattr(dir, name) {
            Ok(()) | Err(Errno::NODATA | Errno::OPNOTSUPP) => {}
            Err(e) => return Err(e),
        }
    }
    fs::fchown(
        dir,
        Some(Uid::from_raw(owner.uid)),
        Some(Gid::from_raw(owner.gid)),
    )?;
    fs::fchmod(dir, Mode::from_raw_mode(mode))
}

/// Copy the directory `sn` in `sp` to `dn` in `dp`, `owner`'s, built in
/// [`STAGING`] beside it and put in place at once. With `dn` there
/// already, the two are swapped and what was there is removed: an empty
/// directory, or the same state when it's handed in place.
fn copy(
    sp: &OwnedFd,
    sn: &CStr,
    dp: &OwnedFd,
    dn: &CStr,
    owner: Owner,
    shown: &str,
) -> Result<(), Refused> {
    let fail = failed(shown);
    // One a copy cut short left.
    match remove(dp, STAGING) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(e) => return Err(fail(e)),
    }
    fs::mkdirat(dp, STAGING, Mode::from_raw_mode(0o700)).map_err(&fail)?;
    let staged = fs::openat(dp, STAGING, dir_flags(), Mode::empty()).map_err(&fail)?;
    // This process's (root's) alone until it's whole, and nothing it
    // holds takes the parent's default ACL.
    let us = Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    };
    own(&staged, us, 0o700).map_err(&fail)?;
    let src = fs::openat(sp, sn, dir_flags(), Mode::empty()).map_err(&fail)?;
    let copied = tree(&src, &staged, owner, Path::new(shown));
    if let Err(e) = copied {
        let _ = remove(dp, STAGING);
        return Err(e);
    }
    let placed = durable(&staged)
        .and_then(|()| own(&staged, owner, 0o700))
        .and_then(
            |()| match fs::renameat_with(dp, STAGING, dp, dn, RenameFlags::NOREPLACE) {
                Err(Errno::EXIST) => {
                    fs::renameat_with(dp, STAGING, dp, dn, RenameFlags::EXCHANGE)?;
                    remove(dp, STAGING)
                }
                other => other,
            },
        )
        .and_then(|()| durable(dp));
    placed.map_err(fail)
}

/// Flush `fd` to disk: a file's contents, or a directory's entries. Each
/// directory the copy makes is flushed once it's whole, as is each file,
/// so nothing it holds is lost to a crash once the old copy is removed.
fn durable(fd: impl AsFd) -> Result<(), Errno> {
    #[cfg(test)]
    {
        let st = fs::fstat(&fd)?;
        tests::SYNCED.with(|s| s.borrow_mut().insert((st.st_dev, st.st_ino)));
    }
    fs::fsync(fd)
}

/// Copy what `src` holds into `dst`, each entry made afresh, `owner`'s.
fn tree(src: &OwnedFd, dst: &OwnedFd, owner: Owner, at: &Path) -> Result<(), Refused> {
    for entry in fs::Dir::read_from(src).map_err(failed(&at.display().to_string()))? {
        let entry = entry.map_err(failed(&at.display().to_string()))?;
        let name = entry.file_name().to_owned();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let path = at.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
        let shown = path.display().to_string();
        let fail = failed(&shown);
        let unsafe_entry =
            |what: &str| Refused::Refused(format!("{shown}: {what}, which the flow never writes"));
        // Opened once, never through a link: a fifo doesn't block, a
        // socket won't open.
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let fd = match fs::openat(src, &name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => continue,
            Err(Errno::LOOP) => return Err(unsafe_entry("a link")),
            Err(Errno::NXIO) => return Err(unsafe_entry("a socket")),
            Err(e) => return Err(fail(e)),
        };
        let st = fs::fstat(&fd).map_err(&fail)?;
        let mode = Mode::from_raw_mode(st.st_mode & 0o777);
        let times = Timestamps {
            last_access: Timespec {
                tv_sec: st.st_atime,
                tv_nsec: st.st_atime_nsec as _,
            },
            last_modification: Timespec {
                tv_sec: st.st_mtime,
                tv_nsec: st.st_mtime_nsec as _,
            },
        };
        match FileType::from_raw_mode(st.st_mode) {
            FileType::Directory => {
                fs::mkdirat(dst, &name, Mode::from_raw_mode(0o700)).map_err(&fail)?;
                let made = fs::openat(dst, &name, dir_flags(), Mode::empty()).map_err(&fail)?;
                tree(&fd, &made, owner, &path)?;
                own(&made, owner, mode.bits()).map_err(&fail)?;
                fs::futimens(&made, &times).map_err(&fail)?;
                durable(&made).map_err(&fail)?;
            }
            FileType::RegularFile if st.st_nlink == 1 => {
                // A process of the flow's holds it: Conary's pass and
                // worker locks, a worker's `.running`.
                match fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
                    Ok(()) => {}
                    Err(Errno::WOULDBLOCK) => {
                        return Err(Refused::Busy(format!(
                            "{shown}: locked, so a process of the flow's still runs as the user it had: \
                             start burrow-flow-data.service (or a unit of the flow's) again once it has exited"
                        )));
                    }
                    Err(e) => return Err(fail(e)),
                }
                let out_flags = OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC;
                let out =
                    fs::openat(dst, &name, out_flags, Mode::from_raw_mode(0o600)).map_err(&fail)?;
                let mut from = std::fs::File::from(fd);
                let mut to = std::fs::File::from(out);
                std::io::copy(&mut from, &mut to)
                    .and_then(|_| to.flush())
                    .map_err(|e| Refused::Failed(format!("{shown}: {e}")))?;
                own_file(&to, owner, mode).map_err(&fail)?;
                fs::futimens(&to, &times).map_err(&fail)?;
                durable(&to).map_err(&fail)?;
            }
            FileType::RegularFile => return Err(unsafe_entry("a file linked from elsewhere too")),
            _ => return Err(unsafe_entry("neither a directory nor a file")),
        }
    }
    Ok(())
}

fn own_file(file: &std::fs::File, owner: Owner, mode: Mode) -> Result<(), Errno> {
    fs::fchown(
        file,
        Some(Uid::from_raw(owner.uid)),
        Some(Gid::from_raw(owner.gid)),
    )?;
    fs::fchmod(file, mode)
}

/// Remove the directory `name` in `parent` and all it holds, never
/// through a link. One pass: what's made in it meanwhile leaves it there,
/// and says so.
fn remove(parent: &OwnedFd, name: &CStr) -> Result<(), Errno> {
    let dir = match fs::openat(parent, name, dir_flags(), Mode::empty()) {
        Ok(dir) => dir,
        // A link, or a file: the name alone goes.
        Err(Errno::LOOP | Errno::NOTDIR) => return fs::unlinkat(parent, name, AtFlags::empty()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in fs::Dir::read_from(&dir)? {
        let name = entry?.file_name().to_owned();
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name);
        }
    }
    for name in names {
        match remove(&dir, &name) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) => return Err(e),
        }
    }
    fs::unlinkat(parent, name, AtFlags::REMOVEDIR)
}

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
