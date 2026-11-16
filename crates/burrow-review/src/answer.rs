//! Taking a protected answer. Walk from `/` with directory descriptors:
//! no symlink traversal or pathname replacement between a check and read.
//! System ancestors may belong to root; the answer directory and file
//! must belong to the reviewer. No component may be writable by others.
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{Mode, OFlags, openat};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// SHA-256 of the exact queued bytes, including their formatting.
pub fn digest(request: &[u8]) -> String {
    Sha256::digest(request)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Publish with a private temporary file, then a read-only group grant.
/// An asker's umask cannot make an answer writable by that asker.
pub fn save(path: &Path, value: &Value) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut tmp = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| io::Error::other("answer needs a directory"))?,
    )?;
    tmp.write_all(&serde_json::to_vec_pretty(value)?)?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o640))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn check(file: &File, uid: u32, exact_owner: bool) -> io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != uid && (exact_owner || m.uid() != 0) {
        return Err(io::Error::other("protected answer has a foreign owner"));
    }
    if m.mode() & 0o022 != 0 {
        return Err(io::Error::other(
            "protected answer path is writable by others",
        ));
    }
    Ok(())
}

/// Read an answer only if its entire path is protected and its request
/// digest matches. Missing files are the only condition callers retry.
/// `uid` comes from the reviewer's system account, never from an answer.
pub fn read(path: &Path, uid: u32, request: &[u8]) -> io::Result<Value> {
    if !path.is_absolute() {
        return Err(io::Error::other("protected answer path must be absolute"));
    }
    read_from(File::open("/")?, path, uid, request)
}

/// Read beneath an already established trust boundary. The caller must
/// supply a trusted directory descriptor; every component beneath it still
/// gets ownership, mode and symlink checks, and the answer's digest is held
/// to the exact request. The absolute-path entry point always starts at `/`.
pub fn read_from(mut dir: File, path: &Path, uid: u32, request: &[u8]) -> io::Result<Value> {
    check(&dir, uid, false)?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(io::Error::other("non-normal protected answer path"));
        };
        let last = components.peek().is_none();
        let flags = OFlags::RDONLY
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | if last {
                OFlags::empty()
            } else {
                OFlags::DIRECTORY
            };
        let mut file = File::from(openat(&dir, name, flags, Mode::empty())?);
        check(&file, uid, last || components.clone().count() == 1)?;
        if last {
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("protected answer is not a regular file"));
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let answer: Value = serde_json::from_slice(&bytes)?;
            if answer["digest"].as_str() != Some(digest(request).as_str()) {
                return Err(io::Error::other("protected answer request digest differs"));
            }
            return Ok(answer);
        }
        dir = file;
    }
    Err(io::Error::other("protected answer path has no file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, u32) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answer.json");
        fs::write(
            &path,
            serde_json::json!({"digest":digest(b"request")}).to_string(),
        )
        .unwrap();
        let uid = fs::metadata(&path).unwrap().uid();
        assert!(read_fixture(&path, uid, b"request").is_ok());
        (dir, path, uid)
    }

    fn read_fixture(path: &Path, uid: u32, request: &[u8]) -> io::Result<Value> {
        read_from(
            File::open(path.parent().unwrap())?,
            Path::new(path.file_name().unwrap()),
            uid,
            request,
        )
    }

    #[test]
    fn public_reader_refuses_writable_ancestors() {
        let (dir, path, uid) = fixture();
        burrow_test::permissions(dir.path(), 0o777);
        // The absolute reader may reject an earlier ancestor first (for
        // example a build directory owned by another user). It must refuse
        // the path in either case. Isolate the intended mode failure with
        // the private descriptor boundary, so its exact cause is still held.
        assert!(read(&path, uid, b"request").is_err());
        assert!(
            read_fixture(&path, uid, b"request")
                .unwrap_err()
                .to_string()
                .contains("writable")
        );
    }

    #[test]
    fn refuses_foreign_owned_answer() {
        let (_dir, path, uid) = fixture();
        let error = read_fixture(&path, uid + 1, b"request").unwrap_err();
        assert!(error.to_string().contains("foreign owner"));
        // Also exercise the file check independently of its ancestors.
        let error = check(&File::open(path).unwrap(), uid + 1, true).unwrap_err();
        assert!(error.to_string().contains("foreign owner"));
    }

    #[test]
    fn refuses_writable_directory() {
        let (dir, path, uid) = fixture();
        for mode in [0o770, 0o707] {
            burrow_test::permissions(dir.path(), mode);
            assert!(
                read_fixture(&path, uid, b"request")
                    .unwrap_err()
                    .to_string()
                    .contains("writable")
            );
        }
    }

    #[test]
    fn refuses_mismatched_digest() {
        let (_dir, path, uid) = fixture();
        assert!(
            read_fixture(&path, uid, b"other request")
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
    }

    #[test]
    fn refuses_writable_file_and_symlink() {
        let (dir, path, uid) = fixture();
        burrow_test::permissions(&path, 0o660);
        assert!(read_fixture(&path, uid, b"request").is_err());
        burrow_test::permissions(&path, 0o640);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_fixture(&link, uid, b"request").is_err());
    }
}
