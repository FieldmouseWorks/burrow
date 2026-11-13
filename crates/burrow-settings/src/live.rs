//! How a live settings document is read and changed: every change
//! serialized under the settings lock, logged and synced, then published by
//! an atomic rename that's synced before anyone is told it happened.

use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// A document a request's receipt can live in, with the change it made.
pub(crate) trait Document: Default + Serialize + DeserializeOwned {
    fn receipts(&mut self) -> &mut BTreeMap<String, String>;
}

/// True only for an accessible, existing path; false only for genuine
/// absence. Inspect ancestors too: ENOENT behind a dangling link isn't absence.
pub fn path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                path_exists(parent)?;
            }
            Ok(false)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The document at PATH; its default when there's none yet.
pub(crate) fn read<D: DeserializeOwned + Default>(path: &Path) -> Result<D, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !path_exists(path)? => {
            Ok(D::default())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// PATH, and its name in its folder, on disk.
fn sync(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("a settings file needs a parent directory")?;
    File::open(path)
        .and_then(|f| f.sync_all())
        .and_then(|()| File::open(parent).and_then(|f| f.sync_all()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// One change to the document at PATH, under LOCK, logged to LOG. CHANGE
/// returns None to leave it as it is, or the change's answer. A receipt
/// already in the document answers again with what it said, changing
/// nothing, once that is on disk: an answer is only ever given for a change
/// that's durable.
pub(crate) fn write<D: Document>(
    path: &Path,
    lock: &Path,
    log: &Path,
    actor: &str,
    receipt: Option<&str>,
    args: &[String],
    change: impl FnOnce(&mut D) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let parent = path.parent().ok_or("overlay needs a parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mut state: D = read(path)?;
    if let Some(body) = receipt.and_then(|r| state.receipts().get(r).cloned()) {
        sync(path)?;
        return Ok(Some(body));
    }
    let Some(answer) = change(&mut state)? else {
        if path_exists(path)? {
            sync(path)?;
        }
        return Ok(None);
    };
    if let Some(receipt) = receipt {
        let said = if answer.is_empty() {
            "Settings changed; use settings show for the current policy."
        } else {
            &answer
        };
        state.receipts().insert(receipt.into(), said.into());
    }
    let mut log = File::options()
        .append(true)
        .create(true)
        .open(log)
        .map_err(|e| e.to_string())?;
    writeln!(
        log,
        "{}",
        serde_json::json!({"at": crate::now(), "actor": actor, "change": args})
    )
    .map_err(|e| e.to_string())?;
    log.sync_all().map_err(|e| e.to_string())?;
    let pending = path.with_extension("pending");
    let mut file = File::create(&pending).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&state).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    // Readers include the isolated reviewer account. This overlay holds
    // public operational policy, regardless of the writer's umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(pending, path).map_err(|e| e.to_string())?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(Some(answer))
}
