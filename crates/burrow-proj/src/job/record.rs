//! How a job's protected records ([`super::Request`], [`super::Schedule`])
//! are kept in its host control directory: written whole through a staging
//! file, first adopted without overwriting a record that landed first, and
//! published to the job directory as a copy nothing proj decides by.
use super::Job;
use crate::read;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Write `bytes` to a staging file created exclusively beside `path`: a name
/// that exists already (another writer's, or an orphan a crash left, which
/// may alias the live record) is never opened, and a fresh one is tried.
fn stage(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    use io::Write;
    static N: AtomicU64 = AtomicU64::new(0);
    loop {
        let staged = path.with_file_name(format!(
            "{}.new.{}.{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes) {
                    let _ = fs::remove_file(&staged);
                    return Err(e);
                }
                return Ok(staged);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

fn staged<T: Serialize>(job: &Job, path: &Path, value: &T) -> Result<PathBuf, String> {
    crate::ask::ensure(job)?;
    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    stage(path, json.as_bytes()).map_err(|e| e.to_string())
}

/// The record at `path`; None when there's none yet.
pub(super) fn load<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match crate::state_reads::bytes(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Initialize the record at `path` with `value` if no one has, and return
/// the winner with whether it's `value`: a reader holding stale values never
/// overwrites a record, or an authorized update, that landed after it looked.
pub(super) fn adopt<T: Serialize + DeserializeOwned>(
    job: &Job,
    path: &Path,
    value: T,
) -> Result<(T, bool), String> {
    let staged = staged(job, path, &value)?;
    let linked = fs::hard_link(&staged, path);
    let _ = fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok((value, true)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let winner = load(path)?.ok_or_else(|| format!("{} vanished", path.display()))?;
            Ok((winner, false))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Replace the record at `path` whole.
pub(super) fn save<T: Serialize>(job: &Job, path: &Path, value: &T) -> Result<(), String> {
    let staged = staged(job, path, value)?;
    fs::rename(&staged, path).map_err(|e| e.to_string())
}

/// Put the job directory's copy back as `files` has it: each file's whole
/// text, or None for one that isn't there.
pub(super) fn publish<'k>(
    job: &Job,
    files: impl IntoIterator<Item = (&'k str, Option<String>)>,
) -> io::Result<()> {
    for (key, value) in files {
        let path = job.path(key);
        if read(&path).as_deref() == value.as_deref().map(|v| v.trim_end_matches('\n')) {
            continue;
        }
        match value {
            Some(value) => crate::state_reads::write_public(&path, value)?,
            None => match crate::state_reads::remove(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            },
        }
    }
    Ok(())
}
