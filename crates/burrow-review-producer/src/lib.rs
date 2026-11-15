//! Conary's review plugin and its producer (Conary's `docs/m6-plugins.md`,
//! R).
//!
//! The flow runs the plugin ([`plugin`]), a thin client from
//! burrow-producer's kit: it answers `describe` and Conary's review fixture
//! itself and forwards each review request to the producer
//! ([`producer`]). The producer checks the request against the PR on
//! GitHub, takes a snapshot of the head as the steward's reviewers read
//! one (burrow-upkeep's, secrets paths left out), briefs it with
//! burrow-brief's review brief, and has the reviewers' service
//! (burrow-review) run a fresh Codex look at it: the service is the one
//! place Codex's login is lent from, so the look is queued there rather
//! than run here. Its answer is the look's findings, by Codex's own
//! session, on the model and effort that ran. What it looked at is kept
//! ([`look`]).
//!
//! The flow publishes the review itself. The sign pass ([`sign`]), which
//! burrow-events starts when a review is submitted, finds each kept look's
//! publication and signs `review_receipt(head, body)` as
//! `conary/review/ROLE` only once everything the look recorded still holds.
//!
//! - [`history`]: a role's earlier findings on a PR, with the replies they
//!   got, as the brief quotes them.
pub mod history;
pub mod look;
pub mod producer;
pub mod sign;

use conary_core::plugin::{Answer, Capabilities, Identity, Kind, Model, Sandbox, Task};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The engine the plugin names itself by, and the one its looks run on:
/// the reviewers' service runs Codex alone.
pub const ENGINE: &str = "codex";

/// The reviewer the reviewers' service runs a look as.
pub const REVIEWER: &str = "codex";

/// What the module tells the plugin, the producer and the sign pass.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The producer's socket, in its own directory under the producers'.
    pub socket: PathBuf,
    /// The flow's controller user, the one account that may ask.
    pub controller: String,
    /// The producer's name, as the machine's producers file has it, and
    /// the systemd credential its key comes as (the sign pass's alone).
    pub producer: String,
    pub credential: String,
    /// The review role the producer answers for, as the flow's settings
    /// name it (`settings.reviewers`, which must name it alone: a request
    /// doesn't say which role it's for), and as the settings source names
    /// the role whose model and effort a look runs on when the flow
    /// chooses none.
    pub role: String,
    /// The flow's configurations, one a repo (`OWNER/NAME.json`,
    /// lowercased): its review and trusted authors, read for every request.
    pub flow: PathBuf,
    /// The settings source: what Codex offers, and the role's tuning.
    pub settings: burrow_settings::Config,
    /// The steward's burrow-upkeep config: the paths no reviewer reads, and
    /// whom the reviewers review for.
    pub upkeep: PathBuf,
    /// gh as the App the producer acts as on GitHub.
    pub gh: PathBuf,
    /// The producer's own state: `looks/` (what each look read and said,
    /// which the sign pass's group may change), and the snapshots' mirrors
    /// and what each kept of its base (`review/`, burrow-upkeep's).
    pub state: PathBuf,
    /// The sign pass's own state, its user's alone to write: `records/`,
    /// `issued/` (each publication it signed), and `wake.json`, where it
    /// books its next pass (burrow-events') while a look's publication is
    /// still to come or something failed.
    pub sign: PathBuf,
    /// The snapshots the reviewers' service reads a fresh look from, one a
    /// look, which the producer takes and clears (burrow-review's `fresh`).
    pub fresh: PathBuf,
    /// The reviewers' service's queue and the answers it leaves.
    pub queue: PathBuf,
    pub done: PathBuf,
    /// Seconds a look waits for its publication before it's let go.
    pub keep: u64,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A repo's flow configuration, read now.
    pub fn flow(&self, repo: &str) -> Result<conary_github::flow::Config, String> {
        let path = self.flow.join(format!("{}.json", repo.to_lowercase()));
        let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The plugin the flow runs, forwarding to the producer at `cfg.socket`. It
/// offers what Codex offers on this machine, read when it's asked.
pub fn plugin(cfg: &Config) -> Result<burrow_producer::plugin::Plugin, String> {
    Ok(burrow_producer::plugin::Plugin {
        describe: describe(cfg)?,
        by: Identity {
            engine: ENGINE.into(),
            session: None,
        },
        fixture,
        socket: cfg.socket.clone(),
    })
}

/// A model that reviews: Codex's models and efforts, as the settings
/// source's offers list them. Its look runs in the reviewers' service's
/// sandbox, whose one way out is its API.
pub fn describe(cfg: &Config) -> Result<Answer, String> {
    let settings = cfg.settings.role_view()?.in_force;
    let offer = settings.offers.get(ENGINE).cloned().unwrap_or_default();
    Ok(Answer::Describe {
        versions: vec![conary_core::plugin::CONTRACT.into()],
        kinds: vec![Kind::Review],
        v2_kinds: vec![],
        v4_kinds: vec![],
        models: offer
            .models
            .iter()
            .map(|id| Model {
                id: id.clone(),
                efforts: offer.efforts.clone(),
                context_tokens: None,
                max_output_tokens: None,
            })
            .collect(),
        capabilities: Some(Capabilities {
            sandbox: Sandbox::Brings,
            network: true,
            devices: vec![],
            mcp: false,
        }),
        meters: vec![],
    })
}

/// Conary's review fixture, answered with no look: nothing to find.
pub fn fixture(task: &Task) -> Option<Answer> {
    matches!(task, Task::Review { .. }).then(|| Answer::Review { findings: vec![] })
}

/// The tree of commit `head` in `repo`, as GitHub has it.
pub(crate) fn tree_of(
    api: &mut dyn burrow_github::Api,
    repo: &str,
    head: &str,
) -> Result<String, String> {
    Ok(burrow_github::call(
        api,
        "GET",
        &format!("repos/{repo}/git/commits/{head}"),
        None,
        None,
    )?
    .json()?["tree"]["sha"]
        .as_str()
        .filter(|t| id(t))
        .ok_or_else(|| format!("GitHub names no tree for {head}"))?
        .to_owned())
}

/// Whether `s` is a commit's full id, as git writes it.
pub(crate) fn id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `repo` is OWNER/NAME.
pub(crate) fn named(repo: &str) -> bool {
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| plain(owner) && plain(name))
}

/// The time now, in seconds since the epoch.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// SHA-256 of `text`, in hex.
pub(crate) fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
