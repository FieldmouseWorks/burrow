//! The reviewers' service's config, as modules/autoland.nix writes it:
//! REVIEW_CONFIG names the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Option<burrow_settings::Config>,
    /// The looks the steward's passes queue, a file each (burrow-upkeep's
    /// queue.rs), which this takes off once it has answered.
    pub queue: PathBuf,
    /// Each look's answer, under its queue entry's name, for the repo's next
    /// pass to post.
    pub done: PathBuf,
    /// Each reviewer's session per open PR, which every look at it resumes;
    /// the steward's sweep marks a closed PR's gone, and this deletes it.
    pub sessions: PathBuf,
    /// The snapshots' checkouts, by name: what a look reads.
    pub snapshots: PathBuf,
    pub reviewers: Reviewers,
    /// How long one look may take, in minutes.
    pub timeout_minutes: f64,
    /// Where the agents' commands are.
    pub agents: PathBuf,
    /// bubblewrap, which each look runs in.
    pub sandbox: PathBuf,
    #[serde(rename = "box")]
    pub sandboxed: Sandboxed,
    /// The repos whose looks go first (`leadRepos`), then the rest of the
    /// pipeline's own (`pipelineRepos`): the steward's, any case.
    pub lead_repos: Vec<String>,
    pub pipeline_repos: Vec<String>,
    /// burrow-events' kick for the steward's passes:
    /// `http://127.0.0.1:PORT/kick/autoland`, a repo's under it.
    pub kick: String,
    /// The file this was read from, and what it said then: the deployed
    /// copy, which a switch replaces under a running service (load).
    #[serde(skip)]
    pub source: Option<(PathBuf, String)>,
}

/// Just the admission policy of a deployed config, which may be a newer
/// one's, with fields this one doesn't know.
#[derive(Deserialize)]
struct Deployed {
    #[serde(default)]
    settings: Option<burrow_settings::Config>,
}

/// The reviewers this machine has: Codex is the only one.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewers {
    pub codex: Reviewer,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewer {
    /// Whether the machine has it review at all (`reviewers.codex.enable`):
    /// off, nothing it was queued before is sent to it either.
    pub enable: bool,
    /// How hard it thinks: codex's `model_reasoning_effort`.
    pub effort: String,
    /// At most this many of its looks at once.
    pub parallel: usize,
    /// The repos it leaves alone (a repo's `codex = false`), as the
    /// steward's config names them.
    pub off: Vec<String>,
}

/// What a look's sandbox shows it of the machine.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sandboxed {
    /// A file of store paths, one a line: the closure of what it runs on,
    /// never the whole store.
    pub store: PathBuf,
    /// The machine's CA bundle.
    pub ca_bundle: PathBuf,
    /// Links it makes: where, to what (the machine's /bin/sh, say).
    pub links: BTreeMap<String, String>,
}

impl Config {
    /// Whether the repo's looks wait: the admission policy deployed now,
    /// with its runtime overlay, decides, not the one the service started
    /// with. One it can't read pauses everything.
    pub fn paused(&self, repo: &str) -> bool {
        let deployed;
        let settings = match &self.source {
            None => self.settings.as_ref(),
            Some((path, _)) => {
                deployed = fs::read(path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Deployed>(&b).ok())
                    .and_then(|d| d.settings);
                deployed.as_ref()
            }
        };
        settings.is_none_or(|s| !s.allows(repo).unwrap_or(false))
    }

    /// Whether a switch has deployed another config since this one was
    /// read: the next run is the new one's, so this one starts no more
    /// looks (work.rs). One it can't read now hasn't moved.
    pub fn moved(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|(path, was)| fs::read_to_string(path).is_ok_and(|now| now != *was))
    }

    /// The config REVIEW_CONFIG names.
    pub fn load() -> Result<Config, String> {
        let path = std::env::var_os("REVIEW_CONFIG").ok_or("REVIEW_CONFIG names no config")?;
        Config::read(Path::new(&path))
    }

    /// The config at path, which it reads again for what's deployed since.
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg = Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.source = Some((path.to_owned(), text));
        Ok(cfg)
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// The reviewer queued under this name (its entries' prefix), with the
    /// name it goes by, if this machine has it.
    pub fn reviewer(&self, name: &str) -> Option<(&'static str, &Reviewer)> {
        match name {
            "codex" => Some(("Codex", &self.reviewers.codex)),
            _ => None,
        }
    }

    /// Where a repo's looks go in line, first to last: 0 for a lead repo, 1
    /// for the rest of the pipeline's own, 2 for everything else. The
    /// steward ranks its Claude jobs the same way (common.py `rank`).
    pub fn rank(&self, repo: Option<&str>) -> u8 {
        let Some(repo) = repo else { return 2 };
        let is = |list: &[String]| list.iter().any(|x| x.eq_ignore_ascii_case(repo));
        if is(&self.lead_repos) {
            0
        } else if is(&self.pipeline_repos) {
            1
        } else {
            2
        }
    }
}
