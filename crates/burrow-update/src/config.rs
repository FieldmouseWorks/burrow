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
    /// runuser, which runs them as `user`, and the PATH they run with:
    /// git's, gh's, nix's, proj's and the system's, for Claude Code. Its
    /// own, so a run started through sudo runs them as the service does.
    pub runuser: PathBuf,
    pub path: String,
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
    /// Where its clone comes from: the flake's repo.
    pub origin: String,
    /// The operator's checkout of the flake, which a run brings along to
    /// main when it isn't mid-work.
    pub flake: PathBuf,
    /// The user's group, which owns what it writes into the clone.
    pub group: String,
    /// Who hears about what it can't fix, by name.
    pub owner: String,
    /// Claude Code's release manifest in the flake, when it pins Claude
    /// Code, and where Claude Code's releases are.
    pub manifest: Option<String>,
    pub releases: String,
    /// The VM tests an update passes before it goes live.
    pub checks: Vec<String>,
    /// The dead man's switch window for an update.
    pub within: String,
    /// Where the switch keeps its state: a switch is pending while it has a
    /// `pending` file.
    pub switch_state: PathBuf,
    /// The system that runs (/run/current-system).
    pub current_system: PathBuf,
    /// What keeps it to one run at a time.
    pub lock: PathBuf,
    /// The timers `status` lists.
    pub timers: Vec<String>,
    pub tools: Tools,
}

/// What a run runs as root, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// What a run that isn't root starts again through.
    pub sudo: PathBuf,
    /// HOST-switch, HOST-confirm and HOST-boot (the trial boot, when the
    /// machine has one): the dead man's switch.
    pub switch: PathBuf,
    pub confirm: PathBuf,
    pub boot: Option<PathBuf>,
    /// HOST-health and HOST-notify.
    pub health: PathBuf,
    pub notify: PathBuf,
    pub vulnix: PathBuf,
    /// What fetches Claude Code's releases, and puts its manifest in the
    /// clone as the user's.
    pub curl: PathBuf,
    pub install: PathBuf,
    /// systemd-cat, for a warning in the journal, and systemctl.
    pub logger: PathBuf,
    pub systemctl: PathBuf,
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
            "user": "sam", "home": "/home/sam", "uid": 1000, "runuser": "/b/runuser", "path": "/b",
            "configuration": "box", "system": "x86_64-linux", "heavy": "/b/heavy",
            "project": "homelab", "app": "workers",
            "train": {"repo": "sam/homelab", "branch": "box-update", "settled": "review-settled",
                      "context": "conary/train", "steward": "sams-steward"},
            "origin": "https://github.com/sam/homelab", "flake": "/home/sam/homelab",
            "group": "users", "owner": "Sam", "manifest": "claude-code.json",
            "releases": "https://downloads.example/claude", "checks": ["agents"],
            "within": "15min", "switchState": "/run/box-switch", "currentSystem": "/run/current-system",
            "lock": "/run/box-update.lock", "timers": ["burrow-events-catch-up.timer"],
            "tools": {"sudo": "/b/sudo", "switch": "/b/box-switch", "confirm": "/b/box-confirm",
                      "boot": "/b/box-boot", "health": "/b/box-health", "notify": "/b/box-notify",
                      "vulnix": "/b/vulnix", "curl": "/b/curl", "install": "/b/install",
                      "logger": "/b/systemd-cat", "systemctl": "/b/systemctl"},
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.train.unwrap().steward, "sams-steward");
        assert_eq!(
            (cfg.heavy.unwrap().to_str(), cfg.app.as_deref()),
            (Some("/b/heavy"), Some("workers"))
        );
        assert_eq!(
            (cfg.manifest.as_deref(), cfg.tools.boot.unwrap().to_str()),
            (Some("claude-code.json"), Some("/b/box-boot"))
        );
        // Outside a train, on a machine without heavy slots, an App, Claude
        // Code pinned or a trial boot.
        for k in ["train", "heavy", "app", "manifest"] {
            config[k] = serde_json::Value::Null;
        }
        config["tools"]["boot"] = serde_json::Value::Null;
        std::fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert!(cfg.train.is_none() && cfg.heavy.is_none() && cfg.app.is_none());
        assert!(cfg.manifest.is_none() && cfg.tools.boot.is_none());
        config["tools"]["git"] = "/b/git".into();
        std::fs::write(&path, config.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `git`")
        );
        config["tools"].as_object_mut().unwrap().remove("git");
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
        assert!(Config::read(&path).unwrap().checks.is_empty());
        config["tools"].as_object_mut().unwrap().remove("vulnix");
        std::fs::write(&path, config.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("missing field `vulnix`")
        );
        assert!(Config::read(&dir.path().join("none.json")).is_err());
    }
}
