//! The guard's config, as modules/claude.nix writes it: the hook passes its
//! path as the one argument.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// git, to see what a push with no refspec would push.
    pub git: PathBuf,
    /// zfs, which takes the undo snapshots.
    pub zfs: PathBuf,
    /// systemd-run, through which the user's service manager takes them for
    /// a guard outside the machine's own user namespace (undo.rs's
    /// [`Namespace`](crate::Namespace)).
    pub systemd_run: PathBuf,
    /// What gets an undo snapshot before a command that deletes or rewrites
    /// files (burrow.undo.datasets); none, and nothing is snapshotted.
    pub snapshot_datasets: Vec<String>,
    /// How long the guard takes before it asks instead: past the hook's
    /// timeout, Claude Code runs the command unchecked.
    pub ask_after_seconds: u64,
    /// The label that puts work ahead of the line, which only the owner
    /// puts on (burrow.autoland.labels.first; first.rs): a proj job's
    /// command that would is refused. None where nothing reads it.
    #[serde(default)]
    pub first_label: Option<String>,
}

/// The user the guard runs as, by the name /etc/passwd gives its uid; None
/// when that can't be read.
#[must_use]
pub fn current_user() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").ok()?.uid().to_string();
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            (fields.nth(1)? == uid).then(|| name.to_owned())
        })
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
