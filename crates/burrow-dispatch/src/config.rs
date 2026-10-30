//! The dispatcher's config, as modules/dispatch.nix writes it: DISPATCH_CONFIG
//! names the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Its state: state.json, watch.json, wake.json and the pass's lock.
    pub state_dir: PathBuf,
    /// burrow-events' kick port: `http://127.0.0.1:PORT/kick`.
    pub kick: String,
    /// The stall watchdog's handler, kicked when the watch file's items
    /// change; None without a watchdog.
    pub watchdog: Option<String>,
    /// This handler's own name, for the waits a pass of its own moves on.
    pub handler: String,
    /// The Actions runners on this machine, by unit.
    pub runners: BTreeMap<String, Runner>,
    /// Seconds a runner may sit idle with jobs queued for it before it's
    /// restarted.
    pub quiet: f64,
    /// HOST-update, when the machine updates itself.
    pub deploy: Option<Deploy>,
    /// HOST-switch's state directory: a switch is in flight while it holds
    /// `pending`, and its revert unit, while that runs.
    pub switch: Option<Switch>,
}

/// A self-hosted runner, as it was registered.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    /// OWNER/REPO, whose jobs it takes.
    pub repo: String,
    /// Every label it has: the ones it was registered with, and the
    /// defaults (self-hosted, its OS and its architecture) unless it was
    /// registered without them.
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deploy {
    /// HOST-update's unit, and the burrow-events handler that starts it.
    pub unit: String,
    pub handler: String,
    /// What its last run said (status.json).
    pub status: PathBuf,
    /// The machine's own flake's repo (OWNER/REPO), when HOST-update watches
    /// it: the running system's commit of it is its configuration revision,
    /// read as a pass starts, so that a build from a dirty tree and the same
    /// tree committed stay the same system.
    pub own: Option<String>,
    /// Each of the flake's inputs whose default branch it puts live
    /// (OWNER/REPO), and the commit of it the running system was built from.
    pub running: BTreeMap<String, String>,
    /// Seconds a commit may wait to go live, and a run of HOST-update may
    /// take.
    pub after: f64,
    pub run: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switch {
    pub state: PathBuf,
    pub revert: String,
}

impl Config {
    pub fn load() -> Result<Config, String> {
        let path = std::env::var_os("DISPATCH_CONFIG").ok_or("DISPATCH_CONFIG isn't set")?;
        let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.to_string_lossy()))
    }
}
