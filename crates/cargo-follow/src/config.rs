//! What modules/updates/follow.nix tells a pass, as JSON.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The manifests whose `metadata.follow` it keeps.
    pub manifests: Vec<Manifest>,
    /// Its clones, cargo's home and the lock every pass holds.
    pub work: PathBuf,
    /// The handler's wake file (burrow-events), {repo: epoch} with "" for
    /// the full pass: a pass GitHub's rate limit held off is booked there
    /// for when it lifts.
    pub wake: PathBuf,
    /// Labels on an open PR that keep a pass from pushing to it: the
    /// train's, whose candidate a new head would throw away.
    pub frozen: Vec<String>,
}

/// A manifest: its repo, OWNER/NAME, and its path there.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub repo: String,
    pub path: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_what_it_knows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = serde_json::json!({
            "manifests": [{"repo": "sam/home", "path": "crates/Cargo.toml"}],
            "work": "/w", "wake": "/w/wake.json", "frozen": ["waiting: train"],
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let read = Config::read(&path).unwrap();
        assert_eq!(read.manifests[0].path, "crates/Cargo.toml");
        config["dry_run"] = true.into();
        std::fs::write(&path, config.to_string()).unwrap();
        let e = Config::read(&path).unwrap_err();
        assert!(e.contains("unknown field `dry_run`"), "{e}");
    }
}
