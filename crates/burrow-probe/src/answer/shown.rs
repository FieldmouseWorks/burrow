//! What an answer may open, list or resolve on the machine's disk: never a
//! path the steward's reviewers are kept from (`Config::hidden`, their
//! skipPaths), whether it's named directly or reached through a link. A path
//! is walked one component at a time, each directory held open and the
//! next step taken relative to it with O_NOFOLLOW, so nothing swapped in
//! behind a checked directory can redirect a later step. The patterns are
//! the snapshot's own (`burrow_upkeep::snapshot::Hide`: a trailing newline
//! tried bare, a directory matched with a trailing slash and so its
//! descendants), applied to the path as asked, to each step, and to every
//! path a link makes (its target with the rest still to walk), so a name
//! that is hidden is hidden however a link would spell it. A hidden
//! directory is never listed. A file is opened relative to its directory
//! with O_NOFOLLOW, and only when it's a regular file of a bounded size.

use std::fs;
use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;

use regex::Regex;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use super::Machine;

/// The most links one path may pass through.
const MAX_LINKS: usize = 40;

/// Why a path wasn't read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// It, or something on the way to it, isn't there.
    Missing,
    /// The probe's user may not read it, or what leads to it.
    Denied,
    /// It, or a link on the way to it, is a path the reviewers never read.
    Hidden,
    /// Anything else, said as it was.
    Other(String),
}

impl Unread {
    fn of(e: &std::io::Error) -> Unread {
        match e.kind() {
            ErrorKind::NotFound | ErrorKind::NotADirectory => Unread::Missing,
            ErrorKind::PermissionDenied => Unread::Denied,
            _ => Unread::Other(e.to_string()),
        }
    }

    fn errno(e: rustix::io::Errno) -> Unread {
        Unread::of(&std::io::Error::from(e))
    }

    /// As an answer says it.
    pub fn say(&self) -> String {
        match self {
            Unread::Missing => "not there".into(),
            Unread::Denied => "unknown: the probe may not read it".into(),
            Unread::Hidden => {
                "not read: it is, or leads to, a path the reviewers never read".into()
            }
            Unread::Other(why) => format!("unknown: {why}"),
        }
    }
}

/// One command of a directory: its name and, for a link, the file it
/// leads to once every link on the way is followed.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub link: Option<String>,
}

/// A directory's commands, and how many entries were left out.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// The directory's resolved path.
    pub real: String,
    pub entries: Vec<Entry>,
    /// Left out as hidden, itself or by where a link leads.
    pub hidden: usize,
    /// Left out because the probe could not tell what they are.
    pub unread: usize,
}

/// The reviewers' hidden patterns, compiled.
pub(super) struct Guard(Vec<Regex>);

impl Guard {
    /// Whether a pattern finds the path. A pattern's `$` matches only at
    /// the very end, and git allows a name that ends in a newline, so such
    /// a path is tried without it too (the snapshot's own rule).
    fn skipped(&self, path: &str) -> bool {
        let bare = path.strip_suffix('\n');
        self.0
            .iter()
            .any(|r| r.is_match(path) || bare.is_some_and(|b| r.is_match(b)))
    }

    /// The snapshot's rule for one path: matched itself, or in a directory
    /// that is (a directory matches with a trailing slash).
    fn snapshot(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        self.skipped(path)
            || (1..parts.len()).any(|i| self.skipped(&format!("{}/", parts[..i].join("/"))))
    }

    /// Whether `path` is one the reviewers never read; `dir` for a path
    /// that is a directory (so held to a pattern's trailing slash too). A
    /// skipPath is written for a repo's paths, so it's held to the absolute
    /// path, the path without its leading slash, and each tail that starts
    /// after a slash: whatever directory a repo's root would be.
    pub(super) fn hidden(&self, path: &str, dir: bool) -> bool {
        let tails = std::iter::once(path).chain(
            path.match_indices('/')
                .map(|(i, _)| &path[i + 1..])
                .filter(|t| !t.is_empty()),
        );
        tails.into_iter().any(|t| {
            self.snapshot(t) || (dir && !t.ends_with('/') && self.snapshot(&format!("{t}/")))
        })
    }

    /// For a path whose kind isn't known yet: hidden as either.
    fn hidden_any(&self, path: &str) -> bool {
        self.hidden(path, false) || self.hidden(path, true)
    }
}

/// A path walked to its end.
pub(super) struct Resolved {
    /// The path with every link resolved.
    pub real: String,
    /// The last directory on the way, held open: the path's own when it
    /// is one, else the one holding `leaf`.
    pub(super) dir: OwnedFd,
    /// The last component, when it isn't a directory.
    pub(super) leaf: Option<String>,
}

/// `path` with `.` and `..` taken out, lexically.
fn normal<'a>(done: &[String], more: impl IntoIterator<Item = &'a str>) -> String {
    let mut out: Vec<&str> = done.iter().map(String::as_str).collect();
    for p in more {
        match p {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    format!("/{}", out.join("/"))
}

impl Machine<'_> {
    /// The hidden patterns, compiled; an error if any doesn't compile,
    /// so nothing is read rather than read past a pattern meant to hide it.
    pub(super) fn guard(&self) -> Result<Guard, Unread> {
        self.cfg
            .hidden
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<_, _>>()
            .map(Guard)
            .map_err(|_| {
                Unread::Other(
                    "the reviewers' hidden paths don't all compile, so no file is read".into(),
                )
            })
    }

    /// `path` (absolute, as the machine names it) with every link along it
    /// resolved, none of them followed by the system.
    pub(super) fn resolve(&self, path: &str) -> Result<String, Unread> {
        Ok(self.resolve_with(&self.guard()?, path)?.real)
    }

    pub(super) fn resolve_with(&self, g: &Guard, path: &str) -> Result<Resolved, Unread> {
        if !path.starts_with('/') {
            return Err(Unread::Other(format!("{path:?} isn't an absolute path")));
        }
        if g.hidden_any(&normal(&[], path.split('/'))) {
            return Err(Unread::Hidden);
        }
        let root = rustix::fs::open(
            &self.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(Unread::errno)?;
        let mut fds = vec![root];
        let mut todo: Vec<String> = path.split('/').map(str::to_owned).collect();
        todo.reverse();
        let mut done: Vec<String> = Vec::new();
        let mut leaf = None;
        let mut links = 0;
        while let Some(part) = todo.pop() {
            if part.is_empty() || part == "." {
                continue;
            }
            if leaf.is_some() {
                // Something below a file.
                return Err(Unread::Missing);
            }
            if part == ".." {
                if done.pop().is_some() {
                    fds.pop();
                }
                continue;
            }
            let at = fds.last().expect("the root is held");
            let next = normal(&done, [part.as_str()]);
            let below = todo.iter().any(|p| !p.is_empty() && p != ".");
            if g.hidden(&next, below) {
                return Err(Unread::Hidden);
            }
            let st = rustix::fs::statat(at, part.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(Unread::errno)?;
            match FileType::from_raw_mode(st.st_mode) {
                FileType::Symlink => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(Unread::Other(format!("{path}: too many links")));
                    }
                    let target = rustix::fs::readlinkat(at, part.as_str(), Vec::new())
                        .map_err(Unread::errno)?;
                    let target = target.to_string_lossy().into_owned();
                    let base: &[String] = if target.starts_with('/') { &[] } else { &done };
                    // The path this link makes, with the rest still to
                    // walk: held to the patterns before it's resolved away.
                    let rest = todo.iter().rev().map(String::as_str);
                    let made = normal(base, target.split('/').chain(rest));
                    if g.hidden_any(&target) || g.hidden_any(&made) {
                        return Err(Unread::Hidden);
                    }
                    if target.starts_with('/') {
                        done.clear();
                        fds.truncate(1);
                    }
                    todo.extend(target.split('/').rev().map(str::to_owned));
                }
                FileType::Directory => {
                    if g.hidden(&next, true) {
                        return Err(Unread::Hidden);
                    }
                    let fd = rustix::fs::openat(
                        at,
                        part.as_str(),
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(Unread::errno)?;
                    fds.push(fd);
                    done.push(part);
                }
                _ => {
                    leaf = Some(part.clone());
                    done.push(part);
                }
            }
        }
        Ok(Resolved {
            real: format!("/{}", done.join("/")),
            dir: fds.pop().expect("the root is held"),
            leaf,
        })
    }

    /// Whether `full` is a command PATH would run: an executable regular
    /// file once every link on the way is followed. None for a dangling
    /// link, a directory or a file that isn't executable; the file's real
    /// path otherwise.
    fn command(&self, g: &Guard, full: &str) -> Result<Option<String>, Unread> {
        let r = match self.resolve_with(g, full) {
            Ok(r) => r,
            Err(Unread::Missing) => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(leaf) = &r.leaf else {
            return Ok(None);
        };
        let st = match rustix::fs::statat(&r.dir, leaf.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(e) => {
                return match Unread::errno(e) {
                    Unread::Missing => Ok(None),
                    e => Err(e),
                };
            }
        };
        Ok(
            (FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
                && st.st_mode & 0o111 != 0)
                .then_some(r.real),
        )
    }

    /// A directory's commands, by name, each link's file as it resolves
    /// (every step held to the patterns): any entry, or target, that's
    /// hidden is left out and counted, and so is one the probe can't tell.
    pub(super) fn list(&self, dir: &str) -> Result<Listing, Unread> {
        let g = self.guard()?;
        let r = self.resolve_with(&g, dir)?;
        if r.leaf.is_some() {
            return Err(Unread::Other(format!("{dir} isn't a directory")));
        }
        let mut listing = Listing {
            real: r.real.clone(),
            ..Listing::default()
        };
        // Each entry is held to the directory's name as it was asked for
        // (a link's name, however it leads on) as well as its real one.
        let logical = normal(&[], dir.split('/'));
        let read = rustix::fs::Dir::read_from(&r.dir).map_err(Unread::errno)?;
        for entry in read {
            let entry = entry.map_err(Unread::errno)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let full = format!("{}/{name}", r.real.trim_end_matches('/'));
            let asked = format!("{}/{name}", logical.trim_end_matches('/'));
            let kind = entry.file_type();
            let hidden = [&full, &asked].into_iter().any(|p| match kind {
                FileType::Directory => g.hidden(p, true),
                FileType::RegularFile => g.hidden(p, false),
                _ => g.hidden_any(p),
            });
            if hidden {
                listing.hidden += 1;
                continue;
            }
            match self.command(&g, &asked) {
                Ok(Some(real)) => listing.entries.push(Entry {
                    name,
                    link: (kind == FileType::Symlink).then_some(real),
                }),
                Ok(None) => {}
                Err(Unread::Hidden) => listing.hidden += 1,
                Err(_) => listing.unread += 1,
            }
        }
        listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(listing)
    }

    /// Whether `name` is a command in the directory `dir`, as PATH would
    /// run it: the file it leads to, if it's a link. One that is, or leads
    /// to, a hidden path is Hidden. None where nothing there would run.
    pub(super) fn entry(&self, dir: &str, name: &str) -> Result<Option<Entry>, Unread> {
        let g = self.guard()?;
        let real = self.resolve_with(&g, dir)?.real;
        let full = format!("{}/{name}", real.trim_end_matches('/'));
        // Held to the directory's name as asked for too, and walked by it,
        // so every link on the way is checked as a link.
        let logical = normal(&[], dir.split('/'));
        let asked = format!("{}/{name}", logical.trim_end_matches('/'));
        if g.hidden_any(&full) || g.hidden_any(&asked) {
            return Err(Unread::Hidden);
        }
        let Some(file) = self.command(&g, &asked)? else {
            return Ok(None);
        };
        Ok(Some(Entry {
            name: name.to_owned(),
            link: (file != full).then_some(file),
        }))
    }

    /// A small regular file's text: at `path` resolved, opened relative to
    /// the directory that was checked, without following a link, and no
    /// more than `max` bytes.
    pub(super) fn read_small(&self, path: &str, max: u64) -> Result<String, Unread> {
        let g = self.guard()?;
        let r = self.resolve_with(&g, path)?;
        let Some(leaf) = &r.leaf else {
            return Err(Unread::Other(format!("{path} isn't a regular file")));
        };
        let fd = rustix::fs::openat(
            &r.dir,
            leaf.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(Unread::errno)?;
        let file = fs::File::from(fd);
        let meta = file.metadata().map_err(|e| Unread::of(&e))?;
        if !meta.is_file() {
            return Err(Unread::Other(format!("{path} isn't a regular file")));
        }
        if meta.len() > max {
            return Err(Unread::Other(format!("{path} is over {max} bytes")));
        }
        let mut text = String::new();
        file.take(max)
            .read_to_string(&mut text)
            .map_err(|e| Unread::of(&e))?;
        Ok(text)
    }
}
