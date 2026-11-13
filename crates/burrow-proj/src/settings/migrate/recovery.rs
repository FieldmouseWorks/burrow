//! Complete a legacy journal in the legacy store before publishing routing.
//! Keep the completed journal for retries and rollback: its resolved result is
//! then also imported into the role document, atomically with the switch.
use super::{read, text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;

/// A switch the old engines door or CLI had journalled (`roles.operation`):
/// its receipt, the setup it switched to and from, and its answer once done.
#[derive(Serialize, Deserialize)]
pub(super) struct Operation {
    receipt: PathBuf,
    destination: String,
    before: Option<String>,
    pub(super) result: Option<String>,
}

fn sync_parent(path: &Path) -> Result<(), String> {
    File::open(path.parent().ok_or("no parent")?)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, body: &[u8]) -> Result<(), String> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or("no parent")?)
        .map_err(|e| e.to_string())?;
    tmp.write_all(body)
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    tmp.persist(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    sync_parent(path)
}

fn operation(path: &Path) -> Result<Option<Operation>, String> {
    text(path)?
        .map(|t| serde_json::from_str(&t).map_err(|e| format!("{}: {e}", path.display())))
        .transpose()
}

pub(super) fn run(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let journal = dir.join("roles.operation");
    let Some(mut op) = operation(&journal)? else {
        return Ok(BTreeMap::new());
    };
    if let Some(done) = operation(&op.receipt)? {
        op.result = done.result.or(op.result);
    }
    if op.result.is_none() {
        let from = read(dir)?;
        from.setups
            .saved
            .get(&op.destination)
            .ok_or_else(|| format!("no setup {}", op.destination))?;
        let roles = dir.join("roles");
        let target = fs::canonicalize(dir.join("roles.d").join(&op.destination))
            .map_err(|e| e.to_string())?;
        let applied = fs::read_link(&roles)
            .is_ok_and(|_| fs::canonicalize(&roles).ok().as_ref() == Some(&target));
        let previous = if applied && op.before.as_deref() == Some(op.destination.as_str()) {
            // The old switch to an already-active named setup was a no-op;
            // it did not replace the previous pointer with itself.
            text(&dir.join("roles.previous"))?.map(|s| s.trim().to_owned())
        } else if let Some(before) = &op.before {
            Some(before.clone())
        } else if applied {
            // An unsaved predecessor was snapshotted before the roles rename.
            text(&dir.join("roles.previous"))?
                .map(|s| s.trim().to_owned())
                .filter(|name| name.starts_with("unsaved-") && from.setups.saved.contains_key(name))
        } else if let Some(body) = text(&roles)? {
            // Capture the plain predecessor before changing either pointer.
            let mut i = 0u64;
            let name = loop {
                let name = format!("unsaved-migration-{i}");
                if !burrow_settings::path_exists(&dir.join("roles.d").join(&name))? {
                    break name;
                }
                i += 1;
            };
            write(&dir.join("roles.d").join(&name), body.as_bytes())?;
            Some(name)
        } else {
            None
        };
        if let Some(previous) = &previous {
            write(&dir.join("roles.previous"), previous.as_bytes())?;
        } else if burrow_settings::path_exists(&dir.join("roles.previous"))? {
            fs::remove_file(dir.join("roles.previous")).map_err(|e| e.to_string())?;
            sync_parent(&dir.join("roles.previous"))?;
        }
        if !applied {
            let staging = tempfile::tempdir_in(dir).map_err(|e| e.to_string())?;
            let pending = staging.path().join("roles");
            symlink(target, &pending).map_err(|e| e.to_string())?;
            fs::rename(pending, &roles).map_err(|e| e.to_string())?;
        }
        sync_parent(&roles)?;
        // The saved predecessor includes all tuning: models apply to every
        // legacy setup, and read() imports them into each Roles snapshot.
        op.result = Some(format!(
            "Was active: {}\nActive: {}\n",
            op.before.as_deref().unwrap_or("unsaved"),
            op.destination
        ));
    }
    let body = serde_json::to_vec(&op).map_err(|e| e.to_string())?;
    // These legacy writes describe an applied legacy switch. A new door is
    // gated until the same result is committed inside the live document.
    write(&op.receipt, &body)?;
    write(&journal, &body)?;
    Ok([(
        op.receipt.to_string_lossy().into_owned(),
        op.result.ok_or("no result")?,
    )]
    .into())
}
