//! The state directory's files: JSON, each written whole
//! ([`burrow_process::replace`]), readable by the service's user alone.

use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The file's value, or None when there's no file.
pub fn load<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the value in place of the file, all of it or none, readable by the
/// service's user alone: [`burrow_process::replace::replace`].
pub fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    burrow_process::replace::replace(path, bytes)
}

/// Remove the file; one that's already gone is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saves_whole_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        assert_eq!(load::<Vec<u8>>(&path).unwrap(), None);
        save(&path, &vec![1u8, 2]).unwrap();
        assert_eq!(load::<Vec<u8>>(&path).unwrap(), Some(vec![1, 2]));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!path.with_extension("tmp").exists());
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert_eq!(load::<Vec<u8>>(&path).unwrap(), None);
    }

    #[test]
    fn a_symlink_where_the_new_file_goes_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let precious = dir.path().join("precious");
        fs::write(&precious, "keep").unwrap();
        let path = dir.path().join("x.json");
        std::os::unix::fs::symlink(&precious, path.with_extension("tmp")).unwrap();
        save(&path, &1u8).unwrap();
        assert_eq!(fs::read_to_string(&precious).unwrap(), "keep");
        assert_eq!(load::<u8>(&path).unwrap(), Some(1));
    }

    #[test]
    fn a_garbled_file_is_an_error_not_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        fs::write(&path, "{").unwrap();
        assert!(load::<Vec<u8>>(&path).is_err());
    }
}
