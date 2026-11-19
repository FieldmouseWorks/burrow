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
    /// What gets an undo snapshot before a command that deletes or rewrites
    /// files (burrow.undo.datasets); none, and nothing is snapshotted.
    pub snapshot_datasets: Vec<String>,
    /// How long the guard takes before it asks instead: past the hook's
    /// timeout, Claude Code runs the command unchecked.
    pub ask_after_seconds: u64,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
