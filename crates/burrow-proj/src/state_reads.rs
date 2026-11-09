//! Job state is data, never authority to follow another host path.
use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsRawFd,
    path::{Component, Path},
};

/// Pin every component without symlinks, check the pinned inode before opening
/// it for reading, and bound the read. Renames cannot redirect either open.
pub(crate) fn bytes(path: &Path) -> io::Result<Vec<u8>> {
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
    let file = File::open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))?;
    const LIMIT: u64 = 64 * 1024 * 1024;
    let mut out = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut out)?;
    if out.len() as u64 > LIMIT {
        return Err(io::Error::other("state file exceeds read limit"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
