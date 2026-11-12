//! The one way a program writes a state file whole: a reader sees the old
//! bytes or the new ones, never half, and a crash after the call returns
//! cannot leave the file empty. Every state write in the workspace goes
//! through here; `crates/clippy.toml` fails the build on a second copy.
use std::{
    fs::{File, Permissions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};

/// Mode of a record only its writer's account may read.
pub const PRIVATE: u32 = 0o600;
/// Mode of a record other accounts read (a status, a queue, a deadline).
pub const SHARED: u32 = 0o644;

/// Write `bytes` to `path` whole, readable by its owner only.
pub fn replace(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    replace_mode(path, bytes, PRIVATE)
}

/// Write `bytes` to `path` whole, with `mode` at the moment it appears (the
/// writer's umask does not apply).
pub fn replace_mode(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>, mode: u32) -> io::Result<()> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("state has no name"))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    publish(dir, name.as_ref(), bytes.as_ref(), mode)
}

/// The core, shared with the confined `Dir::replace`: a temporary with a
/// random name in `dir`, written, synced, renamed over `dir/name`, then the
/// directory synced.
pub(crate) fn publish(dir: &Path, name: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut temporary = tempfile::Builder::new()
        .prefix(".state-")
        .tempfile_in(dir)?;
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    #[allow(clippy::disallowed_methods, reason = "the helper itself")]
    temporary.persist(dir.join(name)).map_err(|e| e.error)?;
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests;
