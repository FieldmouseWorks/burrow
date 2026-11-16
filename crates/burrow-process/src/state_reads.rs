//! Confined job-state reads and recovery writes. Guest paths never authorize
//! following a link to another host file or directory.
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
        leaf_name(name)?;
        self.create_new(name)?.write_all(value)
    }
    pub fn create_new(&self, name: &str) -> io::Result<File> {
        leaf_name(name)?;
        Ok(File::from(fs::openat2(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )?))
    }
    pub fn remove(&self, name: &str) -> io::Result<()> {
        leaf_name(name)?;
        Ok(fs::unlinkat(&self.0, name, fs::AtFlags::empty())?)
    }
    /// Create a fresh, uniquely named subdirectory and return it pinned.
    pub fn mkdir_unique(&self, prefix: &str) -> io::Result<(String, Self)> {
        if prefix.contains(['/', '\0']) {
            return Err(io::Error::other("state mutation needs a single plain name"));
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for i in 0..1000u32 {
            let name = format!("{prefix}{:x}{:x}{i}", std::process::id(), seed);
            leaf_name(&name)?;
            match fs::mkdirat(&self.0, name.as_str(), Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => {
                    let dir = self.dir(Path::new(&name))?;
                    return Ok((name, dir));
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(io::Error::other("no free directory name"))
    }
    /// Move a child into another pinned directory; both ends stay confined.
    pub fn rename_into(&self, name: &str, to: &Self, to_name: &str) -> io::Result<()> {
        leaf_name(name)?;
        leaf_name(to_name)?;
        Ok(fs::renameat(&self.0, name, &to.0, to_name)?)
    }
    /// Commit absence before a caller retires another recovery record. An
    /// already absent name still needs the pinned directory synced.
    pub fn remove_durable(&self, name: &str) -> io::Result<()> {
        match self.remove(name) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        File::open(format!("/proc/self/fd/{}", self.0.as_raw_fd()))?.sync_all()
    }
    /// Open or create a pinned regular lock file; never follow an alias.
    pub fn read_lock(&self, name: &str) -> io::Result<File> {
        match self.create(name, b"") {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        self.file(Path::new(name))
    }
    pub fn append_file(&self, name: &str) -> io::Result<File> {
        use std::os::unix::fs::MetadataExt;
        let pinned = self.pin(Path::new(name))?;
        if pinned.metadata()?.nlink() != 1 {
            return Err(io::Error::other("writable state must have one link"));
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
    }
}
/// A mutation target is exactly one plain name: nothing nested, so no
/// intermediate component is ever resolved (or followed) before the change.
fn leaf_name(name: &str) -> io::Result<()> {
    // Judge the raw spelling: `Path::components` hides trailing `/.`, repeated
    // separators and `.` parts, any of which lets a symlink be followed.
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(io::Error::other("state mutation needs a single plain name"));
    }
    Ok(())
}
fn child_name(path: &Path) -> io::Result<()> {
    if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::other(
            "state paths must stay beneath their directory",
        ));
    }
    Ok(())
}

/// Atomically replace a record relative to a pinned directory. Neither the
/// destination nor a predictable staging name is ever opened for writing.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?;
    parent.replace(
        path.file_name()
            .ok_or_else(|| io::Error::other("state has no name"))?
            .as_ref(),
        bytes,
    )
}

/// Open a transcript for append without following any alias, creating it only
/// when absent. A concurrently renamed pathname cannot redirect the pinned fd.
pub fn append(path: &Path) -> io::Result<File> {
    let parent = Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("state has no name"))?;
    match parent.create_new(
        name.to_str()
            .ok_or_else(|| io::Error::other("invalid state name"))?,
    ) {
        Ok(file) => std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/proc/self/fd/{}", file.as_raw_fd())),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => parent.append_file(
            Path::new(name)
                .to_str()
                .ok_or_else(|| io::Error::other("invalid state name"))?,
        ),
        Err(e) => Err(e),
    }
}

impl Dir {
    pub fn replace(&self, name: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_visibility(name, bytes, false)
    }
    /// Public non-secret records are readable at the atomic publication point.
    pub fn replace_public(&self, name: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_visibility(name, bytes, true)
    }
    fn replace_visibility(&self, name: &Path, bytes: &[u8], public: bool) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        child_name(name)?;
        if name.components().count() != 1 {
            return Err(io::Error::other("replacement requires a single filename"));
        }
        let directory = format!("/proc/self/fd/{}", self.0.as_raw_fd());
        let mut temporary = tempfile::Builder::new()
            .prefix(".state-")
            .tempfile_in(&directory)?;
        if public {
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        // Both rename endpoints stay beneath this same pinned directory even
        // if the guest concurrently renames or symlinks its original pathname.
        temporary
            .persist(Path::new(&directory).join(name))
            .map_err(|e| e.error)?;
        File::open(&directory)?.sync_all()?;
        Ok(())
    }
}

pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    replace(path.as_ref(), bytes.as_ref())
}

/// Atomically publish a deliberately public record, with mode 0644 before
/// its rename. Use write for private state; this never exposes a private record.
pub fn write_public(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let parent = Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?;
    parent.replace_public(
        path.file_name()
            .ok_or_else(|| io::Error::other("state has no name"))?
            .as_ref(),
        bytes.as_ref(),
    )
}

pub fn remove(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    let parent = Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?;
    parent.remove(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid state name"))?,
    )
}

/// Durably unlink a state record without following parent aliases. Missing
/// records still sync their parent directory before returning.
pub fn remove_durable(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?
    .remove_durable(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid state name"))?,
    )
}

/// Rename within or across job directories, each end through its pinned
/// parent, so a planted link at either parent or name is never followed.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (from.as_ref(), to.as_ref());
    let split = |p: &Path| -> io::Result<(Dir, String)> {
        let dir = Dir::open(
            p.parent()
                .ok_or_else(|| io::Error::other("state has no parent"))?,
        )?;
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid state name"))?;
        Ok((dir, name.to_owned()))
    };
    let (fd, fname) = split(from)?;
    let (td, tname) = split(to)?;
    fd.rename_into(&fname, &td, &tname)
}

pub fn directory(path: &Path) -> io::Result<Dir> {
    let parent = Dir::open(
        path.parent()
            .ok_or_else(|| io::Error::other("state has no parent"))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("invalid state name"))?;
    match fs::mkdirat(
        &parent.0,
        Path::new(name),
        Mode::RUSR | Mode::WUSR | Mode::XUSR,
    ) {
        Ok(()) => (),
        Err(rustix::io::Errno::EXIST) => (),
        Err(e) => return Err(e.into()),
    }
    parent.dir(Path::new(name))
}

#[cfg(test)]
mod tests;
