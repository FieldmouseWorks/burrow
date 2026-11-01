//! What a pass is told: modules/updates.nix writes it for the machine.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Where it keeps what it has seen and its log (state.rs).
    pub state: PathBuf,
    /// The projects' checkouts, a directory each: osv-scanner reads every
    /// one that's a git checkout of its own.
    pub src: PathBuf,
    /// Who the scans of the projects and the jobs run as.
    pub user: String,
    /// The machine's own project, the flake's checkout in `src`: what KEV
    /// and vulnix find goes to its job.
    pub project: String,
    /// The machine's host name, as its job is told.
    pub host: String,
    /// The NixOS channel it follows (nixos-26.05).
    pub channel: String,
    /// HOST-update, which deploys a fix.
    pub update: String,
    /// The flake's checkout.
    pub flake: String,
    /// Who decides what a job can't.
    pub owner: String,
    /// HOST-notify, as a job runs it.
    pub notify: String,
    /// HOST-boot, on a machine whose kernels get a trial boot.
    pub boot: Option<String>,
    /// Only the merge train moves the flake's main, so a fix goes up as a PR.
    pub train: bool,
    /// The flake's checks an update must pass.
    pub checks: Vec<String>,
    /// The machine's Nix system (x86_64-linux).
    pub system: String,
    /// The GitHub App its jobs act as (`proj run --app`), on a machine with one.
    pub app: Option<String>,
    /// Where CISA's Known Exploited Vulnerabilities list is, tried in turn.
    pub kev: Vec<String>,
    pub tools: Tools,
}

/// The programs a pass runs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// How a pass started as anyone but root gets root (main.rs).
    pub sudo: PathBuf,
    /// How it runs a scan or a job as `user`.
    pub runuser: PathBuf,
    pub nix_store: PathBuf,
    pub vulnix: PathBuf,
    pub osv_scanner: PathBuf,
    /// proj, which starts the jobs.
    pub proj: PathBuf,
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
            "state": "/s", "src": "/src", "user": "sam", "project": "box", "host": "box",
            "channel": "nixos-26.05", "update": "box-update", "flake": "/src/box",
            "owner": "Sam", "notify": "box-notify", "boot": null, "train": false,
            "checks": ["rehearsal"], "system": "x86_64-linux", "app": "box-bot",
            "kev": ["https://example.org/kev.json"],
            "tools": {
                "sudo": "/w/sudo", "runuser": "/b/runuser", "nixStore": "/b/nix-store",
                "vulnix": "/b/vulnix", "osvScanner": "/b/osv-scanner", "proj": "/b/proj",
            },
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let read = Config::read(&path).unwrap();
        assert_eq!(
            (read.boot, read.app.as_deref(), read.tools.osv_scanner),
            (None, Some("box-bot"), PathBuf::from("/b/osv-scanner"))
        );
        config["tools"]["osv_scanner"] = "/b/osv-scanner".into();
        std::fs::write(&path, config.to_string()).unwrap();
        let e = Config::read(&path).unwrap_err();
        assert!(e.contains("unknown field `osv_scanner`"), "{e}");
    }
}
