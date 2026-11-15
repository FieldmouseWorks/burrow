//! Conary's proof plugin, its producer and its worker share one
//! configuration ([`Config`]), which the module writes. The plugin is a
//! thin client from burrow-producer's kit: it describes a program (no
//! models, Conary refuses a proof plugin that offers one), answers Conary's
//! proof and prepare fixtures itself, and forwards every other request to
//! the producer ([`crate::producer`]).
use conary_core::plugin::{
    Answer, Capabilities, CheckResult, Identity, Kind, Sandbox, Task, Verdict,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The engine the plugin's answers are by: a program, never a model.
pub const ENGINE: &str = "burrow-proof";

/// What the module tells the plugin, the producer and the worker.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The producer's socket, in its own directory under the producers'.
    pub socket: PathBuf,
    /// The flow's controller user, the one account that may ask.
    pub controller: String,
    /// The producer's name, as the machine's producers file has it, and
    /// the systemd credential its key comes as.
    pub producer: String,
    pub credential: String,
    /// The flow's configurations, one a repo (`OWNER/NAME.json`,
    /// lowercased), whose `settings.proofs` holds each check's recipe: read
    /// for every request, never taken from it.
    pub flow: PathBuf,
    /// gh as the App the producer acts as on GitHub, and git.
    pub gh: PathBuf,
    pub git: PathBuf,
    /// Where repos are fetched from: `GITHUB/OWNER/REPO.git`.
    pub github: String,
    /// The producer's own state: `records/`, `git/` (its mirrors) and
    /// `runs/`: each run's folder, the only part of it a run's worker sees
    /// (`run`), and writes only its log in.
    pub state: PathBuf,
    /// The worker's unit template: a run is `UNIT@ID.service`.
    pub unit: String,
    pub systemctl: PathBuf,
    /// Seconds a run's unit may take, its setup included.
    pub timeout: u64,
    /// The worker's tools: nix's programs' directory, the heavy slot's
    /// command (the words before the command it holds a slot for), and
    /// where a run's clone goes.
    pub nix: PathBuf,
    pub heavy: Option<Vec<String>>,
    pub trees: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A run's own folder, which only the producer writes.
    pub fn run(&self, id: &str) -> PathBuf {
        self.state.join("runs").join(id)
    }

    /// A repo's mirror, which only the producer writes.
    pub fn mirror(&self, repo: &str) -> PathBuf {
        self.state.join("git").join(format!("{repo}.git"))
    }

    /// A repo's flow configuration.
    pub fn flow(&self, repo: &str) -> PathBuf {
        self.flow.join(format!("{}.json", repo.to_lowercase()))
    }

    /// A run's log, which the producer makes and its worker writes.
    pub fn log(&self, id: &str) -> PathBuf {
        self.run(id).join("log")
    }

    /// A run's own copy of its repo's mirror, the one repo its worker sees.
    pub fn repo(&self, id: &str) -> PathBuf {
        self.run(id).join("repo.git")
    }

    /// A run's home, which its worker makes as its own user: where its
    /// programs keep what they cache (nix's fetches, cargo's), as nix
    /// won't use a home that isn't its user's.
    pub fn home(&self, id: &str) -> PathBuf {
        self.trees.join(format!("{id}.home"))
    }
}

/// The plugin the flow runs, forwarding to the producer at `cfg.socket`.
pub fn plugin(cfg: &Config) -> burrow_producer::plugin::Plugin {
    burrow_producer::plugin::Plugin {
        describe: describe(),
        by: Identity {
            engine: ENGINE.into(),
            session: None,
        },
        fixture,
        socket: cfg.socket.clone(),
    }
}

/// A program that proves and prepares: no models. It brings its own
/// sandbox (the worker's unit, nix's builds), fetches from GitHub, and a
/// build may want the machine's devices, which nix's builders, not the
/// plugin, are given.
pub fn describe() -> Answer {
    Answer::Describe {
        versions: vec![conary_core::plugin::CONTRACT.into()],
        kinds: vec![Kind::Proof],
        v2_kinds: vec![],
        v4_kinds: vec![Kind::Prepare],
        models: vec![],
        capabilities: Some(Capabilities {
            sandbox: Sandbox::Brings,
            network: true,
            devices: vec![],
            mcp: false,
        }),
        meters: vec![],
    }
}

/// Conary's proof and prepare fixtures, answered with no run: each fixture
/// check says by its name how it comes out, and a fixture's inputs are
/// ready, needing no slot.
pub fn fixture(task: &Task) -> Option<Answer> {
    match task {
        Task::Proof { head, checks, .. } => Some(Answer::Proof {
            results: checks
                .iter()
                .map(|check| CheckResult {
                    check: check.clone(),
                    verdict: if check.ends_with("-fail") {
                        Verdict::Fail
                    } else {
                        Verdict::Pass
                    },
                    why: "a conformance fixture: nothing ran".into(),
                })
                .collect(),
            head: (!head.is_empty()).then(|| head.clone()),
        }),
        Task::Prepare { .. } => Some(Answer::Prepare {
            ready: true,
            slot: false,
            why: "a conformance fixture: nothing to ready".into(),
        }),
        _ => None,
    }
}
