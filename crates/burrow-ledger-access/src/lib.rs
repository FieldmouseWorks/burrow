//! The ledger's access (modules/ledger.nix, `burrow.ledger.controllerUser`):
//! every top-level entry of the ledger's directory is burrow.user's, and
//! with a controller, POSIX ACLs naming that user (never its group, which
//! every producer joins) let the flow sign as burrow.user's writers do. It
//! reads the key and can't write its contents, whose owning group gets
//! nothing however the key was made; it reads and writes the files Conary's
//! writer opens in place ([`WRITABLE`]); the directory's default entries let
//! both writers append to and replace whatever the other made first; a
//! directory in it (etags) names no one. It says what the access is, not
//! what to add to it: no entry names anyone but the controller it has now,
//! so a controller changed or turned off keeps nothing.
//!
//! It runs as root, in a directory the controller may write, so it never
//! goes by a name twice: each entry is opened once without following a
//! link (`O_PATH | O_NOFOLLOW`), its owner set through that descriptor, and
//! its ACLs read and set through `/proc/self/fd`, which leads to that inode
//! and no other. A link the controller puts there, or swaps in between, is
//! made burrow.user's and nothing more: what it leads to is never touched.
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat, XattrFlags};
use rustix::io::Errno;
use rustix::process::Uid;
use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

/// The files Conary's writer opens in place (conary-core's log.rs,
/// conary-github's bases.rs), which the controller reads and writes. The
/// anchor and key.pub are renamed in; the key is read only.
pub const WRITABLE: [&str; 4] = ["lock", "ops.jsonl", "anchor", "merge-bases"];

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";

/// Whose the ledger is, and who else signs it.
#[derive(Clone, Copy, Debug)]
pub struct Access {
    /// burrow.user's uid.
    pub owner: u32,
    /// The flow's controller's, if it has one of its own.
    pub controller: Option<u32>,
}

/// The uid `/etc/passwd`'s text `passwd` gives `name`.
pub fn uid_of(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let mut f = line.split(':');
        (f.next() == Some(name)).then(|| f.nth(1)?.parse().ok())?
    })
}

/// One of the directory's entries (or the directory itself, with no name),
/// held open without following a link.
#[derive(Debug)]
pub struct Pinned {
    pub name: Option<CString>,
    fd: OwnedFd,
}

impl Pinned {
    /// Its inode, as `/proc` leads to it.
    fn proc(&self) -> String {
        format!("/proc/self/fd/{}", self.fd.as_raw_fd())
    }

    fn stat(&self) -> rustix::io::Result<Stat> {
        fs::fstat(&self.fd)
    }
}

/// The directory `dir` and each of its entries, pinned, or `None` when
/// there's no ledger yet. An entry gone since it was listed is left out.
pub fn pin(dir: &Path) -> rustix::io::Result<Option<(Pinned, Vec<Pinned>)>> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match fs::open(dir, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut entries = Vec::new();
    for entry in fs::Dir::read_from(&fd)? {
        let name = entry?.file_name().to_owned();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        match fs::openat(&fd, name.as_c_str(), flags, Mode::empty()) {
            Ok(entry) => entries.push(Pinned {
                name: Some(name),
                fd: entry,
            }),
            Err(Errno::NOENT) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Some((Pinned { name: None, fd }, entries)))
}

/// Set the ledger's access in `dir`, saying what failed through `say`.
/// Whether anything did: an entry that fails doesn't stop the rest.
pub fn set(dir: &Path, access: &Access, say: &mut dyn FnMut(String)) -> bool {
    match pin(dir) {
        Ok(None) => false,
        Ok(Some((dir, entries))) => apply(access, &dir, &entries, say),
        Err(e) => {
            say(format!("{}: {e}", dir.display()));
            true
        }
    }
}

/// [`set`], on what [`pin`] holds.
pub fn apply(
    access: &Access,
    dir: &Pinned,
    entries: &[Pinned],
    say: &mut dyn FnMut(String),
) -> bool {
    let mut failed = false;
    for p in std::iter::once(dir).chain(entries) {
        if let Err(e) = one(access, p) {
            let name = p.name.as_deref().map_or(".".into(), CStr::to_string_lossy);
            say(format!("{name}: {e}"));
            failed = true;
        }
    }
    failed
}

fn one(access: &Access, p: &Pinned) -> rustix::io::Result<()> {
    if p.stat()?.st_uid != access.owner {
        // The descriptor's own inode: a link stays a link, burrow.user's.
        let owner = Some(Uid::from_raw(access.owner));
        fs::chownat(&p.fd, c"", owner, None, AtFlags::EMPTY_PATH)?;
    }
    // Read again: a chown clears set-id bits.
    let st = p.stat()?;
    let kind = FileType::from_raw_mode(st.st_mode);
    if !matches!(kind, FileType::RegularFile | FileType::Directory) {
        return Ok(());
    }
    let dir = kind == FileType::Directory;
    let path = p.proc();
    let now = read(&path, ACCESS)?;
    let default_now = if dir { read(&path, DEFAULT)? } else { None };
    let what = match (p.name.as_deref(), dir) {
        (None, _) => What::Ledger,
        (Some(_), true) => What::Dir,
        (Some(name), false) => What::File(name.to_bytes()),
    };
    let want = want(access, what, st.st_mode, now.as_ref(), default_now.as_ref());
    match (&want.access, &now) {
        (Some(want), now) if now.as_ref() != Some(want) => {
            fs::setxattr(&path, ACCESS, &want.encode(), XattrFlags::empty())?;
        }
        (None, Some(now)) => {
            fs::removexattr(&path, ACCESS)?;
            let mode = stripped(st.st_mode, now);
            fs::chmod(&path, Mode::from_raw_mode(mode))?;
        }
        _ => {}
    }
    match (&want.default, &default_now) {
        (Some(want), now) if now.as_ref() != Some(want) => {
            fs::setxattr(&path, DEFAULT, &want.encode(), XattrFlags::empty())?;
        }
        (None, Some(_)) => fs::removexattr(&path, DEFAULT)?,
        _ => {}
    }
    Ok(())
}

/// The mode an inode with `mode` and the ACL `acl` keeps once that ACL is
/// gone: each class with what it had in effect, and no more. The owning
/// group's bits are its entry's as far as the mask lets them through, so a
/// directory Conary made 0700 under the ledger's default entries (owning
/// group `r-x`, mask `---`) stays 0700, where `setfacl -b` would make it 0750.
fn stripped(mode: u32, acl: &Acl) -> u32 {
    let mut base = Base::of(mode, Some(acl));
    if let Some(mask) = acl.perm(MASK) {
        base.group &= mask;
    }
    (mode & 0o7000) | base.mode()
}

/// Which of the ledger's entries an inode is.
#[derive(Clone, Copy, Debug)]
enum What<'a> {
    /// The ledger's directory itself.
    Ledger,
    /// A directory in it (etags).
    Dir,
    /// A file in it, by its name.
    File(&'a [u8]),
}

/// The ACLs an entry should have (`None`: none at all).
#[derive(Debug, PartialEq, Eq)]
struct Want {
    access: Option<Acl>,
    default: Option<Acl>,
}

/// What `what`, with `mode` and the access and default ACLs it has now,
/// should have.
fn want(
    access: &Access,
    what: What,
    mode: u32,
    now: Option<&Acl>,
    default_now: Option<&Acl>,
) -> Want {
    let none = Want {
        access: None,
        default: None,
    };
    let Some(cuid) = access.controller else {
        return none;
    };
    let base = Base::of(mode, now);
    let only = |acl| Want {
        access: Some(acl),
        default: None,
    };
    match what {
        // The controller writes it, and what either writer makes in it
        // takes both.
        What::Ledger => {
            let rwx = 7;
            let dbase = default_now.map_or(base, |d| Base::of(0, Some(d)));
            Want {
                access: Some(base.with(&[(cuid, rwx)], base.group | rwx)),
                default: Some(dbase.with(&[(cuid, rwx), (access.owner, rwx)], dbase.group | rwx)),
            }
        }
        What::Dir => none,
        // Set whole: the owner reads and writes it, the controller reads
        // it, and no one else, its owning group included.
        What::File(b"key") => {
            let key = Base {
                user: 6,
                group: 0,
                other: 0,
            };
            only(key.with(&[(cuid, 4)], 4))
        }
        What::File(name) if WRITABLE.iter().any(|w| w.as_bytes() == name) => {
            only(base.with(&[(cuid, 6)], base.group | 6))
        }
        // Anything else keeps the controller's entry its default gave it,
        // and no one else's, through the mask it has.
        What::File(_) => {
            let Some(now) = now else { return none };
            let named = |e: &Entry| matches!(e.tag, USER | GROUP);
            let kept: Vec<Entry> = now
                .0
                .iter()
                .copied()
                .filter(|e| !named(e) || e.tag == USER && e.id == cuid)
                .collect();
            if kept.iter().any(named) {
                only(Acl(kept))
            } else {
                none
            }
        }
    }
}

/// The ACL `name` of the inode at `path`, if it has one.
fn read(path: &str, name: &str) -> rustix::io::Result<Option<Acl>> {
    let mut buf = vec![0u8; 65536];
    match fs::getxattr(path, name, &mut buf[..]) {
        Ok(n) => Acl::decode(&buf[..n]).map(Some).ok_or(Errno::INVAL),
        // A file system without ACLs has none to take away: setting one
        // there still fails.
        Err(Errno::NODATA | Errno::NOTSUP) => Ok(None),
        Err(e) => Err(e),
    }
}

const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;
const NO_ID: u32 = u32::MAX;

/// One entry, as the kernel keeps it: sorted by tag, then id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    tag: u16,
    id: u32,
    perm: u16,
}

/// An ACL in the kernel's form (its xattr, version 2).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Acl(Vec<Entry>);

impl Acl {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let (version, rest) = bytes.split_first_chunk::<4>()?;
        if u32::from_le_bytes(*version) != 2 || rest.len() % 8 != 0 {
            return None;
        }
        let (chunks, _) = rest.as_chunks::<8>();
        let mut entries: Vec<Entry> = chunks
            .iter()
            .map(|e| Entry {
                tag: u16::from_le_bytes([e[0], e[1]]),
                perm: u16::from_le_bytes([e[2], e[3]]),
                id: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            })
            .collect();
        entries.sort();
        Some(Acl(entries))
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = 2u32.to_le_bytes().to_vec();
        for e in &self.0 {
            out.extend(e.tag.to_le_bytes());
            out.extend(e.perm.to_le_bytes());
            out.extend(e.id.to_le_bytes());
        }
        out
    }

    fn perm(&self, tag: u16) -> Option<u16> {
        self.0.iter().find(|e| e.tag == tag).map(|e| e.perm)
    }
}

/// An inode's owner, owning group and other permissions: its ACL's, or
/// its mode's where it has none (with an ACL, the mode's group bits are the
/// mask).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Base {
    user: u16,
    group: u16,
    other: u16,
}

impl Base {
    fn of(mode: u32, acl: Option<&Acl>) -> Self {
        let bits = |shift: u32| ((mode >> shift) & 7) as u16;
        let from = |tag, shift| acl.and_then(|a| a.perm(tag)).unwrap_or(bits(shift));
        Base {
            user: from(USER_OBJ, 6),
            group: from(GROUP_OBJ, 3),
            other: from(OTHER, 0),
        }
    }

    fn mode(&self) -> u32 {
        u32::from(self.user) << 6 | u32::from(self.group) << 3 | u32::from(self.other)
    }

    /// This base, with `named` users each given their perms, through `mask`.
    fn with(&self, named: &[(u32, u16)], mask: u16) -> Acl {
        let mut entries = vec![
            Entry {
                tag: USER_OBJ,
                id: NO_ID,
                perm: self.user,
            },
            Entry {
                tag: GROUP_OBJ,
                id: NO_ID,
                perm: self.group,
            },
            Entry {
                tag: MASK,
                id: NO_ID,
                perm: mask & 7,
            },
            Entry {
                tag: OTHER,
                id: NO_ID,
                perm: self.other,
            },
        ];
        entries.extend(named.iter().map(|&(id, perm)| Entry {
            tag: USER,
            id,
            perm,
        }));
        entries.sort();
        entries.dedup_by_key(|e| (e.tag, e.id));
        Acl(entries)
    }
}

#[cfg(test)]
mod tests;
