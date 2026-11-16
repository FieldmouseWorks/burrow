//! Conary's judge plugin and its producer (Conary's `docs/m6-plugins.md`,
//! J).
//!
//! The flow runs the plugin ([`plugin`]), a thin client from
//! burrow-producer's kit: it answers `describe` and Conary's judge fixture
//! itself and forwards each judge request to the producer.
//!
//! The work is split between two units, since a judgment runs as a proj
//! job, which is the authoring user's, and the keys must be no such
//! user's:
//!
//! - [`producer`]: `autoland-judge`, a user of its own holding the signing
//!   key and the seal key it shares with the flow. Only the flow's
//!   controller may ask it. It holds a request to GitHub (head, base,
//!   description and the judging context, by Conary's own helper),
//!   allocates the judgment's generation, has the runner judge, refuses a
//!   judge session that isn't independent (Conary's `Sessions::accept`),
//!   holds the PR to GitHub again, and publishes: the sealed comment, then
//!   `conary/judgment` over its receipt. What a crash interrupts is
//!   finished at startup from its record, never judged again.
//! - [`runner`]: the judging engine's side, as burrow.user, with no key.
//!   Only the producer's user may ask it. It quotes every review thread,
//!   review body and answer the request's context digests, briefs a proj
//!   job on the judging role with them ([`brief`]), waits on it through its
//!   record and stops it with its request, and answers what the job
//!   decided ([`verdict`]).
pub mod brief;
pub mod discussion;
pub mod producer;
pub mod runner;
pub mod verdict;

use conary_core::plugin::{Answer, Capabilities, Identity, Judgment, Kind, Sandbox, Task};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// What the module tells the plugin, the producer and the runner, one file
/// for all three.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The producer's socket, in its own directory under the producers'.
    pub socket: PathBuf,
    /// The flow's controller user, the one account that may ask the
    /// producer.
    pub controller: String,
    /// The runner's socket, and the producer's user: the one account that
    /// may ask the runner.
    pub runner: PathBuf,
    pub user: String,
    /// The producer's name, as the machine's producers file has it, and
    /// the systemd credentials its signing key and its seal key come as.
    pub producer: String,
    pub credential: String,
    pub seal: String,
    /// The flow's configurations, one a repo (`OWNER/NAME.json`,
    /// lowercased): whom it reads verdicts from, and its seal key.
    pub flow: PathBuf,
    /// The settings source: the judging role's route and its engine's
    /// offers.
    pub settings: burrow_settings::Config,
    /// proj, and what goes before its arguments; its jobs directory, where
    /// a job leaves its answers; the judging role, as the role map names
    /// it; the App a job acts on GitHub as; how long a job's turn may run,
    /// and go quiet.
    pub proj: Vec<String>,
    pub jobs: PathBuf,
    pub role: String,
    pub app: Option<String>,
    pub timeout: String,
    pub idle: String,
    /// Whom a judge sends what it can't decide.
    pub owner: String,
    /// GitHub's API root, and the commands that print a repo's token (the
    /// repo is their last argument) for the producer's and the runner's
    /// reads, through Conary's own client.
    pub api: String,
    pub token: String,
    pub runner_token: String,
    /// gh as the App the producer posts its sealed comments and check runs
    /// as.
    pub gh: PathBuf,
    /// The producer's own state: `records/` and `generations/`.
    pub state: PathBuf,
    /// The runner's: the jobs it waits on, for a restart to stop.
    pub runner_state: PathBuf,
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

    /// The proj the runner starts its judgments with.
    pub fn proj(&self) -> burrow_producer::jobs::Proj {
        burrow_producer::jobs::Proj {
            proj: self.proj.clone(),
            jobs: self.jobs.clone(),
            role: self.role.clone(),
            app: self.app.clone(),
            timeout: self.timeout.clone(),
            idle: self.idle.clone(),
        }
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
            // The job runs in proj's own sandbox and engine, with the network
            // it reads the PR and fetches its head over.
            capabilities: Some(Capabilities {
                sandbox: Sandbox::Brings,
                network: true,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        // What its own answers, describe and the fixture, are by: Conary
        // holds a judgment to a session, so the fixture's names one of its
        // own, which no real judgment's (a job's) ever is.
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

/// Conary's judge fixture, answered with no job: it accepts.
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
