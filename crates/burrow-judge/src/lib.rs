//! Conary's judge plugin and its producer (Conary's `docs/m6-plugins.md`,
//! J).
//!
//! The flow runs the plugin ([`plugin`]), a thin client from
//! burrow-producer's kit: it answers `describe` and Conary's judge fixture
//! itself and forwards each judge request to the producer.
//!
//! The producer, [`producer`], is `autoland-judge`, a user of its own
//! holding the signing key and the seal key it shares with the flow, which
//! only the flow's controller may ask. It holds a request to GitHub (head,
//! base, description and the judging context, by Conary's own helper),
//! allocates the judgment's generation, has a look judge ([`look`]),
//! refuses a judge session that isn't independent (Conary's
//! `Sessions::accept`), holds the PR to GitHub again, and publishes: the
//! sealed comment, then `conary/judgment` over its receipt. What a crash
//! interrupts is finished at startup from its record, never judged again.
//!
//! Each judgment runs in a look of its own, a unit only the producer
//! starts, as a user systemd makes for it ([`worker`]): the judging
//! engine, in a fresh session, on a brief ([`brief`]) and a read-only
//! checkout the producer laid out, with no network but its gate to the
//! engine's API ([`egress`]). Nothing of a look is `burrow.user`'s, the
//! user authoring sessions run as, so none of them can answer in the
//! judge's place. A head no CI reported on is built first, in units of
//! their own as burrow-proof's worker runs a proof ([`build`]), and the
//! look judges what came of it.
pub mod brief;
pub mod build;
pub mod discussion;
pub mod egress;
pub mod look;
pub mod producer;
pub mod worker;

use conary_core::plugin::{Answer, Capabilities, Identity, Judgment, Kind, Sandbox, Task};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One modular engine surface a judge look can dispatch to (`modules/autoland/judge.nix`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// Conary v5 `job` plugin executable configured for restricted read-only judge looks.
    pub plugin: PathBuf,
    /// Per-engine systemd look template unit (`UNIT@ID.service`) that loads
    /// only this engine's login credential.
    pub unit: String,
    /// Systemd credential name carrying this engine's isolated login.
    pub credential: String,
    /// Environment variable name under which the plugin reads its login.
    pub credential_env: String,
    /// Egress host allowlist (exact hosts or leading `*.` wildcards) on port 443.
    pub hosts: Vec<String>,
}

/// What the module tells the plugin, the producer and its looks, one file
/// for all three.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The producer's socket, in its own directory under the producers'.
    pub socket: PathBuf,
    /// The flow's controller user, the one account that may ask the
    /// producer.
    pub controller: String,
    /// The producer's name, as the machine's producers file has it, and
    /// the systemd credentials its signing key and its seal key come as.
    pub producer: String,
    pub credential: String,
    pub seal: String,
    /// The flow's configurations, one a repo (`OWNER/NAME.json`,
    /// lowercased): whom it reads verdicts from, and its seal key.
    pub flow: PathBuf,
    /// The file those configurations name for the seal key
    /// (`judge_key_file`): the flow's controller's copy of the same key
    /// the producer gets as `seal`, which only the controller can read.
    pub flow_key: PathBuf,
    /// The settings source: the judging role's route and its engine's
    /// offers.
    pub settings: burrow_settings::Config,
    /// The judging role, as the role map names it.
    pub role: String,
    /// Whom a judge sends what it can't decide.
    pub owner: String,
    /// GitHub's API root, and the command that prints a repo's token (the
    /// repo is its last argument) for the producer's reads, through
    /// Conary's own client.
    pub api: String,
    pub token: String,
    /// gh as the App the producer posts its sealed comments and check runs
    /// as.
    pub gh: PathBuf,
    /// git, and where it fetches a repo from (`GITHUB/OWNER/NAME.git`).
    pub git: PathBuf,
    pub github: String,
    /// The producer's own state: `records/`, `generations/`, its mirrors
    /// (`git/`) and its looks' folders (`looks/`).
    pub state: PathBuf,
    /// systemctl, and how long a look may run, in seconds.
    pub systemctl: PathBuf,
    pub timeout: u64,
    /// Configured modular engine surfaces a look may dispatch to, keyed by
    /// engine name (for example `claude`, `antigravity`, or any custom plugin).
    pub engines: BTreeMap<String, EngineConfig>,
    /// The builds' unit (`BUILD@ID.service`), each a check of a head no
    /// CI reported on, run before it's judged ([`build`]), and where a
    /// build's clone and home go, in its unit.
    pub build: String,
    pub trees: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// What a build's worker reads of it: burrow-proof's, with no Nix, no
    /// heavy slot and no network, as a build runs a program, offline, and
    /// nothing else.
    pub fn bench(&self) -> burrow_proof::plugin::Bench<'_> {
        burrow_proof::plugin::Bench {
            state: &self.state,
            git: &self.git,
            nix: None,
            heavy: None,
            trees: &self.trees,
            offline: true,
        }
    }

    /// A repo's flow configuration, read now.
    pub fn flow(&self, repo: &str) -> Result<conary_github::flow::Config, String> {
        let path = self.flow.join(format!("{}.json", repo.to_lowercase()));
        let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The plugin the flow runs, forwarding to the producer at `cfg.socket`. It
/// names itself by the engine the judging role is routed to and offers what
/// that engine offers, read when it's asked: Conary holds every answer to
/// both.
pub fn plugin(cfg: &Config) -> Result<burrow_producer::plugin::Plugin, String> {
    let burrow_producer::jobs::Offer { engine, models } =
        burrow_producer::jobs::offered(&cfg.settings, &cfg.role)?;
    Ok(burrow_producer::plugin::Plugin {
        describe: Answer::Describe {
            versions: vec![conary_core::plugin::CONTRACT.into()],
            kinds: vec![Kind::Judge],
            v2_kinds: vec![],
            v4_kinds: vec![],
            models,
            // Each look runs in a unit of its own, whose only way out is
            // its engine's API.
            capabilities: Some(Capabilities {
                sandbox: Sandbox::Brings,
                network: false,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        // What its own answers, describe and the fixture, are by: Conary
        // holds a judgment to a session, so the fixture's names one of its
        // own, which no real judgment's (a look's) ever is.
        by: Identity {
            engine,
            session: Some(FIXTURE_SESSION.into()),
        },
        fixture,
        socket: cfg.socket.clone(),
    })
}

/// The session the plugin's own answers name.
pub const FIXTURE_SESSION: &str = "burrow-judge-fixture";

/// Conary's judge fixture, answered with no look: it accepts.
pub fn fixture(task: &Task) -> Option<Answer> {
    matches!(task, Task::Judge { .. }).then(|| Answer::Judge {
        verdict: Judgment::Pass,
        why: "a conformance fixture: nothing is judged".into(),
    })
}

/// A GitHub to read, as Conary's flow reads one, new for each request.
pub type Read = Box<dyn Fn() -> Result<Box<dyn conary_github::flow::Read>, String> + Send + Sync>;

/// Conary's own GitHub client at `api`, with the token `token` prints for
/// each repo it reads.
pub fn conary(api: &str, token: &str) -> Result<Read, String> {
    conary_github::Tokens::command(token).map_err(|e| e.to_string())?;
    let (api, token) = (api.to_owned(), token.to_owned());
    Ok(Box::new(move || {
        let tokens = conary_github::Tokens::command(&token).map_err(|e| e.to_string())?;
        Ok(Box::new(conary_github::Api::new(&api, tokens)) as Box<dyn conary_github::flow::Read>)
    }))
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

/// What a judge request names, held to its form: a repo, a PR, a full
/// head, a fresh look, a reason and a context. Either side refuses a
/// request that isn't one.
pub(crate) fn held(request: &conary_core::plugin::Request) -> Result<(), String> {
    use conary_core::plugin::{Context, version};
    if version(&request.contract).is_none_or(|v| v < 4) {
        return Err(format!(
            "{} names no PR, reasons or context: a judgment needs conary-plugin v4",
            request.contract
        ));
    }
    let Task::Judge {
        repo,
        pr,
        head,
        why,
        context,
        ..
    } = &request.task
    else {
        return Err("the judge judges alone".into());
    };
    if !named(repo) || *pr == 0 || !id(head) {
        return Err("a judgment names its repo, its PR and its head's full id".into());
    }
    let basis = conary_github::flow::Basis {
        why: why.clone(),
        context: context.clone(),
    };
    if !basis.valid() {
        return Err("a judgment says why it's asked, once each, and the context it's on".into());
    }
    match &request.execution {
        Some(e) if e.context == Context::Fresh => Ok(()),
        _ => Err("a judgment is a fresh session: it never resumes or forks one".into()),
    }
}

#[cfg(test)]
mod tests;
