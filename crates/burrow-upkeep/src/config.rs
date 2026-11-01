//! burrow-upkeep's config, as modules/autoland.nix writes it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The steward's state dir, where each repo's dashboard-REPO.json and
    /// flaky-REPO.json are kept, and the flaky tool's cache under flaky/. A
    /// status run (`--dry`) keeps its own under dry/.
    pub state_dir: PathBuf,
    /// proj, which says how a Claude job stands (`proj state ID`).
    pub proj: PathBuf,
    /// Where the machine's projects are cloned, burrow among them: a
    /// dashboard's job is told so.
    pub projects_dir: String,
    /// The steward's authors (people's logins), its bots by their Apps'
    /// slugs, and its own App's slug, as the steward's config has them.
    pub authors: Vec<String>,
    pub bots: Vec<String>,
    pub app: String,
    /// How long a Claude job may run, in minutes, unless its part says
    /// otherwise: a dashboard's job's.
    pub job_timeout: u64,
    /// The login Renovate runs as, as REST gives it: the machine's own
    /// App's bot; none, one of `authors`.
    pub renovate: Option<String>,
    /// Renovate's dashboards, when they're watched.
    pub dashboard: Option<Dashboard>,
    /// The flaky tests' quarantine, when it's watched.
    pub flaky: Option<Flaky>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dashboard {
    /// The dashboard's title, as Renovate gives it.
    pub title: String,
    /// How long, in days, an update waiting on something sits on it before
    /// it's stuck, and an error or a problem.
    pub wait_days: u64,
    pub error_days: u64,
    /// burrow-events' kick for Renovate, when it runs on this machine: a
    /// job that ticks a box has it read the dashboard now.
    pub renovate_kick: Option<String>,
    /// Where Renovate's unit notes when its runs ended, when it runs on
    /// this machine: `all`, and one file a repo.
    pub renovate_runs: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flaky {
    /// How long a flaky test's job may run, in minutes.
    pub job_timeout: u64,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
