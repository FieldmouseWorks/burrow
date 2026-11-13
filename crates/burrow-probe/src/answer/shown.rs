//! What an answer may open, list or resolve on the machine's disk: never a
//! path the steward's reviewers are kept from (`Config::hidden`, their
//! skipPaths), whether it's named directly or reached through a link. A path
//! is walked one component at a time without following anything: each step,
//! and the target of each link on the way, is held to the hidden patterns
//! before it's looked at, so a link can't lead a read into a hidden file
//! and a hidden directory is never listed. A file is opened at its resolved
//! path with O_NOFOLLOW, and only when it's a regular file of a bounded
//! size.

use std::fs;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::PermissionsExt;

use regex::Regex;

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

/// One entry of a directory: its name and, for a link, where it points.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub link: Option<String>,
}

/// A directory's entries, and how many were left out as hidden.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// The directory's resolved path.
    pub real: String,
    pub entries: Vec<Entry>,
    pub hidden: usize,
}

impl Machine<'_> {
    /// The hidden patterns, compiled; an error if any doesn't compile,
    /// so nothing is read rather than read past a pattern meant to hide it.
    fn patterns(&self) -> Result<Vec<Regex>, Unread> {
        self.cfg
            .hidden
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<_, _>>()
            .map_err(|_| {
                Unread::Other(
                    "the reviewers' hidden paths don't all compile, so no file is read".into(),
                )
            })
    }

    /// Whether `path` is one the reviewers never read. A skipPath is
    /// written for a repo's paths, so it's held to the absolute path, the
    /// path without its leading slash, and each tail that starts after a
    /// slash: whatever directory a repo's root would be.
    pub(super) fn hidden(&self, path: &str) -> Result<bool, Unread> {
        let patterns = self.patterns()?;
        let tails = std::iter::once(path).chain(
            path.match_indices('/')
                .map(|(i, _)| &path[i + 1..])
                .filter(|t| !t.is_empty()),
        );
        let tails: Vec<&str> = tails.collect();
        Ok(patterns
            .iter()
            .any(|re| tails.iter().any(|t| re.is_match(t))))
    }

    /// `path` (absolute, as the machine names it) with every link along it
    /// resolved, none of them followed by the system: each component, and
    /// each link's target, held to the hidden patterns first.
    pub(super) fn resolve(&self, path: &str) -> Result<String, Unread> {
        if !path.starts_with('/') {
            return Err(Unread::Other(format!("{path:?} isn't an absolute path")));
        }
        let mut todo: Vec<String> = parts(path);
        todo.reverse();
        let mut done: Vec<String> = Vec::new();
        let mut links = 0;
        while let Some(part) = todo.pop() {
            match part.as_str() {
                "" | "." => continue,
                ".." => {
                    done.pop();
                    continue;
                }
                _ => {}
            }
            let next = format!(
                "/{}",
                done.iter()
                    .chain([&part])
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("/")
            );
            if self.hidden(&next)? {
                return Err(Unread::Hidden);
            }
            let meta = fs::symlink_metadata(self.path(&next)).map_err(|e| Unread::of(&e))?;
            if meta.file_type().is_symlink() {
                links += 1;
                if links > MAX_LINKS {
                    return Err(Unread::Other(format!("{path}: too many links")));
                }
                let target = fs::read_link(self.path(&next)).map_err(|e| Unread::of(&e))?;
                let target = target.to_string_lossy().into_owned();
                if self.hidden(&target)? {
                    return Err(Unread::Hidden);
                }
                if target.starts_with('/') {
                    done.clear();
                }
                let mut more = parts(&target);
                more.reverse();
                todo.extend(more);
            } else {
                done.push(part);
            }
        }
        Ok(format!("/{}", done.join("/")))
    }

    /// A directory's entries, by name, with each link's target as it reads
    /// (never followed): any entry, or target, that's hidden is left out
    /// and counted.
    pub(super) fn list(&self, dir: &str) -> Result<Listing, Unread> {
        let real = self.resolve(dir)?;
        let mut listing = Listing {
            real: real.clone(),
            ..Listing::default()
        };
        let read = fs::read_dir(self.path(&real)).map_err(|e| Unread::of(&e))?;
        for entry in read {
            let entry = entry.map_err(|e| Unread::of(&e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let full = format!("{}/{name}", real.trim_end_matches('/'));
            if self.hidden(&full)? {
                listing.hidden += 1;
                continue;
            }
            let link = match entry.file_type() {
                Ok(t) if t.is_symlink() => {
                    let target = fs::read_link(entry.path())
                        .map(|t| t.to_string_lossy().into_owned())
                        .ok();
                    match target {
                        Some(t) if self.hidden(&t)? => {
                            listing.hidden += 1;
                            continue;
                        }
                        t => t,
                    }
                }
                _ => None,
            };
            listing.entries.push(Entry { name, link });
        }
        listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(listing)
    }

    /// Whether `name` is in the directory `real` (resolved already) as a
    /// command, an entry of its own (never followed): its link's target, if
    /// it's a link. One that is, or leads to, a hidden path is Hidden.
    pub(super) fn entry(&self, real: &str, name: &str) -> Result<Option<Entry>, Unread> {
        let full = format!("{}/{name}", real.trim_end_matches('/'));
        if self.hidden(&full)? {
            return Err(Unread::Hidden);
        }
        let meta = match fs::symlink_metadata(self.path(&full)) {
            Ok(m) => m,
            Err(e) if Unread::of(&e) == Unread::Missing => return Ok(None),
            Err(e) => return Err(Unread::of(&e)),
        };
        let link = if meta.file_type().is_symlink() {
            let target = fs::read_link(self.path(&full)).map_err(|e| Unread::of(&e))?;
            let target = target.to_string_lossy().into_owned();
            if self.hidden(&target)? {
                return Err(Unread::Hidden);
            }
            Some(target)
        } else if meta.is_dir() || meta.permissions().mode() & 0o111 == 0 {
            // Not a command PATH would run.
            return Ok(None);
        } else {
            None
        };
        Ok(Some(Entry {
            name: name.to_owned(),
            link,
        }))
    }

    /// A small regular file's text: at `path` resolved, opened without
    /// following a link, and no more than `max` bytes.
    pub(super) fn read_small(&self, path: &str, max: u64) -> Result<String, Unread> {
        let real = self.resolve(path)?;
        let flags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK;
        let fd = rustix::fs::open(self.path(&real), flags, rustix::fs::Mode::empty())
            .map_err(|e| Unread::of(&std::io::Error::from(e)))?;
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

fn parts(path: &str) -> Vec<String> {
    path.split('/').map(str::to_owned).collect()
}
