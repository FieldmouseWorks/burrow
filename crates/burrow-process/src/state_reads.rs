//! Job state is data, never authority to follow another host path.
use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsRawFd,
    path::{Component, Path},
};

/// Pin every component without symlinks, check the pinned inode before opening
/// it for reading. Renames cannot redirect either open.
pub fn open(path: &Path) -> io::Result<File> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::other("state paths cannot traverse parents"));
    }
    let base = fs::open(
        if path.is_absolute() { "/" } else { "." },
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let relative = path.strip_prefix("/").unwrap_or(path);
    let pinned = fs::openat2(
        &base,
        relative,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )?;
    let pinned = File::from(pinned);
    if !pinned.metadata()?.is_file() {
        return Err(io::Error::other("state is not a regular file"));
    }
    // Only our own pinned, verified descriptor is reopened through procfs.
    File::open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
}

/// Stream full transcripts without applying the small-record size limit.
pub fn copy(path: &Path, output: &mut dyn io::Write) -> io::Result<u64> {
    io::copy(&mut open(path)?, output)
}

pub fn prefix(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    open(path)?.take(limit).read_to_end(&mut out)?;
    Ok(out)
}

/// Small state records are bounded; transcripts use the confined file directly.
pub fn bytes(path: &Path) -> io::Result<Vec<u8>> {
    read_bytes(open(path)?)
}

fn read_bytes(file: File) -> io::Result<Vec<u8>> {
    const LIMIT: u64 = 64 * 1024 * 1024;
    let mut out = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut out)?;
    if out.len() as u64 > LIMIT {
        return Err(io::Error::other("state file exceeds read limit"));
    }
    Ok(out)
}

/// Bounded UTF-8 state, with the same confinement as transcript reads.
pub fn text(path: &Path) -> io::Result<String> {
    to_text(bytes(path)?)
}
fn to_text(bytes: Vec<u8>) -> io::Result<String> {
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A directory pinned without following any symlink. Every later lookup stays
/// relative to this inode even if an untrusted writer replaces its pathname.
pub struct Dir(File);
impl Dir {
    pub fn open(path: &Path) -> io::Result<Self> {
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(io::Error::other("state paths cannot traverse parents"));
        }
        let base = fs::open(
            if path.is_absolute() { "/" } else { "." },
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Self::at(&base, path.strip_prefix("/").unwrap_or(path))
    }
    fn at(base: &impl std::os::fd::AsFd, name: &Path) -> io::Result<Self> {
        child_name(name)?;
        Ok(Self(File::from(fs::openat2(
            base,
            name,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )?)))
    }
    pub fn dir(&self, name: &Path) -> io::Result<Self> {
        Self::at(&self.0, name)
    }
    pub fn file(&self, name: &Path) -> io::Result<File> {
        let pinned = self.pin(name)?;
        File::open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
    }
    fn pin(&self, name: &Path) -> io::Result<File> {
        child_name(name)?;
        let file = File::from(fs::openat2(
            &self.0,
            name,
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )?);
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("state is not a regular file"));
        }
        Ok(file)
    }
    pub fn text(&self, name: &str) -> io::Result<String> {
        to_text(read_bytes(self.file(Path::new(name))?)?)
    }
    pub fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
        std::fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd()))?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect()
    }
    /// Write an immutable first observation; an existing name is never opened.
    pub fn create(&self, name: &str, value: &[u8]) -> io::Result<()> {
        use std::io::Write;
        child_name(Path::new(name))?;
        let file = fs::openat2(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )?;
        File::from(file).write_all(value)
    }
    pub fn append(&self, name: &str, value: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let pinned = self.pin(Path::new(name))?;
        std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))?
            .write_all(value)
    }
}
fn child_name(path: &Path) -> io::Result<()> {
    if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::other(
            "state paths must stay beneath their directory",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
