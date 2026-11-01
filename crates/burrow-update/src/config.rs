//! What HOST-update's steps are told: modules/updates.nix writes it for the
//! machine.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-update, as the machine names it.
    pub update: String,
    /// The machine's host name.
    pub host: String,
    /// What HOST-update keeps between runs.
    pub state: PathBuf,
    /// Its own clone of the flake, where it bumps and builds.
    pub work: PathBuf,
    /// Who git and gh run as, with that user's home and uid.
    pub user: String,
    pub home: String,
    pub uid: u32,
    /// runuser, which runs them as `user`.
    pub runuser: PathBuf,
    /// The flake's NixOS configuration for the machine, and the machine's
    /// system (x86_64-linux), which its VM tests are under.
    pub configuration: String,
    pub system: String,
    /// What takes one of the machine's heavy slots for a VM test (burrow.cpu's
    /// heavy), when it has them.
    pub heavy: Option<PathBuf>,
    /// The machine's own project, where its fix jobs go (proj run), and the
    /// App they act as on GitHub, when it has one.
    pub project: String,
    pub app: Option<String>,
    /// Its PR's settings, when the flake's PRs land through its merge
    /// train; null otherwise.
    pub train: Option<Train>,
}

/// HOST-update's PR in a train repo (train.rs).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Train {
    /// The flake's repo on GitHub, OWNER/NAME.
    pub repo: String,
    /// The branch its PR comes from.
    pub branch: String,
    /// The steward's own status, which isn't CI.
    pub settled: String,
    /// The train's status, which counts as CI only as a failure from the
    /// steward's App: the head red merged alone.
    pub context: String,
    /// The steward's App, as GraphQL names its bot.
    pub steward: String,
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
            "update": "box-update", "host": "box", "state": "/s", "work": "/w",
            "user": "sam", "home": "/home/sam", "uid": 1000, "runuser": "/b/runuser",
            "configuration": "box", "system": "x86_64-linux", "heavy": "/b/heavy",
            "project": "homelab", "app": "workers",
            "train": {"repo": "sam/homelab", "branch": "box-update", "settled": "review-settled",
                      "context": "conary/train", "steward": "sams-steward"},
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.train.unwrap().steward, "sams-steward");
        assert_eq!(
            (cfg.heavy.unwrap().to_str(), cfg.app.as_deref()),
            (Some("/b/heavy"), Some("workers"))
        );
        // Outside a train, on a machine without heavy slots or an App.
        for k in ["train", "heavy", "app"] {
            config[k] = serde_json::Value::Null;
        }
        std::fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert!(cfg.train.is_none() && cfg.heavy.is_none() && cfg.app.is_none());
        config["train"] = serde_json::json!({"repo": "sam/homelab", "branch": "b", "settled": "s",
            "context": "c", "steward": "x", "base": "main"});
        std::fs::write(&path, config.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `base`")
        );
        config["train"] = serde_json::Value::Null;
        config["checks"] = serde_json::json!([]);
        std::fs::write(&path, config.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `checks`")
        );
        assert!(Config::read(&dir.path().join("none.json")).is_err());
    }
}
