//! burrow-upkeep's config, as modules/autoland.nix writes it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The steward's state dir, where each repo's repo-REPO.json,
    /// dashboard-REPO.json and flaky-REPO.json are kept, and the flaky
    /// tool's cache under flaky/. A status run (`--dry`) keeps its own
    /// under dry/.
    pub state_dir: PathBuf,
    /// proj, which says how a Claude job stands (`proj state ID`).
    pub proj: PathBuf,
    /// Where the machine's projects are cloned, burrow among them: a
    /// dashboard's job is told so.
    pub projects_dir: String,
    /// The steward's authors (people's logins) and its bots by their Apps'
    /// slugs, as the steward's config has them.
    pub authors: Vec<String>,
    pub bots: Vec<String>,
    /// The steward's own App.
    pub app: App,
    /// GitHub's REST API, where the App's tokens are minted (token.rs).
    pub api: String,
    /// The webhooks to burrow-events (hook.rs).
    pub webhook: Webhook,
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
    /// What each repo's settings are kept at (`settle`).
    pub settle: Settle,
    /// The branches nothing needs any more, when they're deleted.
    pub branches: Option<Branches>,
}

/// The steward's App: its bot is SLUG[bot]; its id is the one its tokens
/// are minted as, and the one the steward's own required checks are pinned
/// to (settle.rs).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub slug: String,
    pub id: u64,
}

/// Where every webhook of the steward's sends, and what a repo's sends: the
/// events burrow-events' handlers take.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    pub url: String,
    pub events: Vec<String>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settle {
    /// Whom the owner's label says a PR waits for.
    pub owner: String,
    /// The steward's status, and the merge train's.
    pub context: String,
    pub train_context: String,
    /// Whether a repo whose rules allow it gets the baseline ruleset.
    pub rulesets: bool,
    /// The labels the steward and its jobs put on PRs.
    pub labels: Labels,
    /// Each repo's rules, and the repos whose differ, by OWNER/NAME.
    pub defaults: Rules,
    pub repos: BTreeMap<String, Overrides>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Labels {
    pub hold: String,
    pub owner: String,
    pub first: String,
    pub waiting: Waiting,
}

/// What a PR waits on, where it shows at a glance (prs.py's `show_waiting`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Waiting {
    pub release_age: String,
    pub proof_slot: String,
    pub reviewer: String,
}

/// A repo's rules: whether automation pushes to its default branch
/// (`directPush`: then only deleting it and force pushes are stopped),
/// whether it gets the baseline ruleset, and whether its merge train is on.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rules {
    pub direct_push: bool,
    pub ruleset: bool,
    pub train: bool,
}

/// The rules a repo has of its own: the rest are the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Overrides {
    pub direct_push: Option<bool>,
    pub ruleset: Option<bool>,
    pub train: Option<bool>,
}

impl Settle {
    /// The repo's rules: its own on top of the defaults.
    pub fn rules(&self, repo: &str) -> Rules {
        let d = self.defaults;
        let Some(o) = self.repos.get(repo) else {
            return d;
        };
        Rules {
            direct_push: o.direct_push.unwrap_or(d.direct_push),
            ruleset: o.ruleset.unwrap_or(d.ruleset),
            train: o.train.unwrap_or(d.train),
        }
    }
}

/// Which branches are never deleted, by glob: those of every repo, each
/// repo's own on top (by OWNER/NAME), and the merge train's candidates' in
/// a repo whose train is on (cn makes and deletes them), when there's a
/// train.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Branches {
    pub keep: Vec<String>,
    pub repos: BTreeMap<String, Vec<String>>,
    pub train: Option<String>,
}

impl Branches {
    /// The globs the repo's branches are kept by, its train on or not.
    pub fn keep(&self, repo: &str, train: bool) -> Vec<String> {
        let own = self.repos.get(repo).into_iter().flatten();
        let cn = self.train.iter().filter(|_| train);
        self.keep.iter().chain(own).chain(cn).cloned().collect()
    }
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
