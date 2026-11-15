//! Conary's fix plugin and its broker (Conary's `docs/m6-plugins.md`, F).
//!
//! The flow runs the plugin ([`plugin`]), a thin client from
//! burrow-producer's kit: it answers `describe` and Conary's fix fixtures
//! itself and forwards each fix request to the broker. The broker
//! ([`broker`]) checks the request against the PR on GitHub, starts the fix
//! as a proj job on the fixing role, waits on that job through its record
//! (`proj why --json`), and answers with what the job pushed and said, each
//! checked against the request and against GitHub. It signs nothing and
//! holds no key: a fix is authorship, not evidence, and the next review
//! grades it.
//!
//! - [`broker`]: one request's fix, one live fix a PR, stopped with its
//!   connection, and a restart that stops what it was waiting on.
//! - [`jobs`]: the proj job a fix runs as, through proj's own commands.
//! - [`answers`]: what the job says it did, held to the problems it was
//!   handed.
//! - [`brief`]: what the job is told.
use conary_core::plugin::{
    Answer, Capabilities, FindingAnswer, FixResult, Identity, Kind, Model, Problem, Sandbox, Task,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub mod answers;
pub mod brief;
pub mod broker;
pub mod jobs;

/// What the module tells the plugin and the broker, one file for both.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The broker's socket, in its own directory under the producers'.
    pub socket: PathBuf,
    /// The flow's controller user, the one account that may ask.
    pub controller: String,
    /// The settings source, where the fixing role's route and its engine's
    /// offers are kept: what the plugin describes itself by, read when it's
    /// asked, so a live change to them is what it offers next.
    pub settings: burrow_settings::Config,
    /// proj, and what goes before its arguments.
    pub proj: Vec<String>,
    /// proj's jobs directory, where a job leaves its answers.
    pub jobs: PathBuf,
    /// The fixing role, as the role map names it.
    pub role: String,
    /// The App the job acts on GitHub as, if the machine has one.
    pub app: Option<String>,
    /// How long a fix job's turn may run, and go quiet, as proj takes them.
    pub timeout: String,
    pub idle: String,
    /// gh, which the broker reads GitHub with.
    pub gh: PathBuf,
    /// The broker's own state: the fixes it's waiting on.
    pub state: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The plugin the flow runs, forwarding to the broker at `cfg.socket`. It
/// names itself by the engine the fixing role is routed to, and offers
/// what that engine offers: Conary holds every answer to both, so a fix
/// job that ran elsewhere, or on a model it didn't offer, is refused there.
pub fn plugin(cfg: &Config) -> Result<burrow_producer::plugin::Plugin, String> {
    let settings = cfg.settings.role_view()?.in_force;
    let engine = settings.route(&cfg.role, None).engine.clone();
    let offer = settings.offers.get(&engine).cloned().unwrap_or_default();
    Ok(burrow_producer::plugin::Plugin {
        describe: Answer::Describe {
            versions: vec![conary_core::plugin::CONTRACT.into()],
            kinds: vec![Kind::Fix],
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
            // The job runs in proj's own sandbox and engine, with the network
            // its push needs.
            capabilities: Some(Capabilities {
                sandbox: Sandbox::Brings,
                network: true,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        by: Identity {
            engine,
            session: None,
        },
        fixture,
        socket: cfg.socket.clone(),
    })
}

/// Conary's fix fixtures, answered with no job: nothing is writable on the
/// reserved repository, so nothing is pushed and each finding is answered
/// unfixed.
pub fn fixture(task: &Task) -> Option<Answer> {
    let Task::Fix { problems, .. } = task else {
        return None;
    };
    let answers = findings(problems)
        .map(|problem| FindingAnswer {
            problem,
            fixed: false,
            reply: "a conformance fixture: nothing here can be changed".into(),
        })
        .collect::<Vec<_>>();
    // The plugin speaks v4 alone, where every fix answers its findings,
    // none at all included.
    Some(Answer::Fix {
        result: if answers.is_empty() {
            FixResult::Unable {
                why: "a conformance fixture: no writable repository is supplied".into(),
            }
        } else {
            FixResult::Unchanged
        },
        answers: Some(answers),
    })
}

/// The indexes of `problems` that are review findings: the ones a fix
/// answers, in order.
pub fn findings(problems: &[Problem]) -> impl Iterator<Item = usize> + '_ {
    problems
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Problem::Finding { .. }))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests;
